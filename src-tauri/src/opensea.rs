use crate::error::{AppError, AppResult};
use crate::wallet;
use alloy::primitives::Address;
use alloy::signers::SignerSync;
use reqwest::cookie::Jar;
use reqwest::header::{ACCEPT, ORIGIN, REFERER};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

const SITE_URL: &str = "https://opensea.io";
const GRAPHQL_URL: &str = "https://gql.opensea.io/graphql";
const APP_ID: &str = "os2-web";
const ELIGIBILITY_PERSISTED_QUERY_HASH: &str =
    "e1b54354df0d26d39c6b81429bd5e5d37749eaa4bdc027f987128f8c1e7d2308";
const SIWE_STATEMENT: &str = "Click to sign in and accept the OpenSea Terms of Service (https://opensea.io/tos) and Privacy Policy (https://opensea.io/privacy).";
const CONNECTED_ACCOUNT_HINT_COOKIE: &str = "connected-account-server-hint";
const NONCE_LIMIT: usize = 4 * 1024;
const AUTH_LIMIT: usize = 64 * 1024;
const GQL_LIMIT: usize = 2 * 1024 * 1024;

const COLLECTION_SEARCH_QUERY: &str = r"
query MintCollectionSearch($query: String!) {
  collectionsByQuery(query: $query, limit: 50) {
    __typename
    slug
    address
    chain { identifier networkId }
  }
}
";

const MINT_ACTION_QUERY: &str = r"
query MintActionTimelineQuery(
  $address: Address!
  $fromAssets: [AssetQuantityInput!]!
  $toAssets: [AssetQuantityInput!]!
  $recipient: Address
) {
  swap(
    address: $address
    fromAssets: $fromAssets
    toAssets: $toAssets
    recipient: $recipient
    action: MINT
  ) {
    actions {
      __typename
      ... on TransactionAction {
        transactionSubmissionData {
          to
          data
          value
          chain { networkId identifier }
        }
      }
    }
    errors { __typename }
  }
}
";

const ELIGIBILITY_QUERY: &str = r"
query DropEligibilityQuery($collectionSlug: String!, $address: Address!) {
  dropBySlug(slug: $collectionSlug) {
    __typename
    ... on Erc721SeaDropV1 {
      minterQuantityMinted(minter: $address)
    }
    stages {
      __typename
      stageType
      stageIndex
      isEligible
      eligibleMinterAddress
      maxTotalMintableByWallet
      eligibleMaxTotalMintableByWallet
      startsAt
      eligiblePrice {
        usd
        token {
          unit
          symbol
          contractAddress
          chain { identifier }
        }
      }
      ... on Erc1155SeaDropV2Stage {
        fromTokenId
        toTokenId
        maxTotalMintableByWalletPerToken
        eligibleMaxTotalMintableByWalletPerToken
      }
    }
  }
}
";

fn os_err(msg: impl Into<String>) -> AppError {
    AppError::Other(format!("opensea: {}", msg.into()))
}

fn validate_slug(slug: &str) -> AppResult<()> {
    if slug.is_empty()
        || slug.len() > 200
        || !slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(AppError::Invalid("invalid opensea slug".into()));
    }
    Ok(())
}

fn create_siwe_message(
    domain: &str,
    address: &str,
    uri: &str,
    chain_id: u64,
    nonce: &str,
    issued_at: &str,
) -> String {
    format!(
        "{domain} wants you to sign in with your Ethereum account:\n{address}\n\n{SIWE_STATEMENT}\n\nURI: {uri}\nVersion: 1\nChain ID: {chain_id}\nNonce: {nonce}\nIssued At: {issued_at}"
    )
}

fn collection_url(slug: &str) -> AppResult<Url> {
    validate_slug(slug)?;
    parse_url(SITE_URL)?
        .join(&format!("/collection/{slug}/overview"))
        .map_err(|_| os_err("collection url"))
}

fn origin() -> &'static str {
    SITE_URL
}

fn parse_json_number_u64(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn parse_json_f64(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// OpenSea `startsAt` may be unix seconds, unix ms, or an ISO-8601 string.
fn parse_starts_at_ms(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => {
            let x = n.as_i64()?;
            Some(if x < 10_000_000_000 { x * 1000 } else { x })
        }
        serde_json::Value::String(s) => {
            if let Ok(x) = s.parse::<i64>() {
                return Some(if x < 10_000_000_000 { x * 1000 } else { x });
            }
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.timestamp_millis())
        }
        _ => None,
    }
}

fn parse_url(s: &str) -> AppResult<Url> {
    Url::parse(s).map_err(|_| os_err("invalid url"))
}

/// Format a future unix-ms timestamp as "Opens Sep 28 12:00 UTC" for schedule hints.
fn format_hint_future(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms)
        .map(|d| format!("Opens {}", d.format("%b %d %H:%M UTC")))
        .unwrap_or_else(|| "Scheduled".into())
}

fn deserialize_stage_index<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    parse_json_number_u64(&v)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| serde::de::Error::custom("bad stageIndex"))
}

fn deserialize_null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::de::DeserializeOwned,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct NonceResponse {
    nonce: String,
}

#[derive(Deserialize)]
struct AuthenticationResponse {
    user: AuthenticationUser,
}

#[derive(Deserialize)]
struct AuthenticationUser {
    address: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyRequest<'a> {
    message: ParsedSiweMessage<'a>,
    signature: &'a str,
    chain_arch: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ParsedSiweMessage<'a> {
    domain: &'a str,
    address: &'a str,
    statement: &'static str,
    uri: &'a str,
    version: &'static str,
    chain_id: String,
    nonce: &'a str,
    issued_at: &'a str,
    account_type: &'static str,
}

#[derive(Serialize)]
struct GraphQlRequest<'a, V: Serialize> {
    operation_name: &'a str,
    query: &'a str,
    variables: &'a V,
}

#[derive(Deserialize)]
struct GraphQlEnvelope<T> {
    data: Option<T>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct GraphQlError {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct CollectionSearchData {
    #[serde(
        rename = "collectionsByQuery",
        default,
        deserialize_with = "deserialize_null_default"
    )]
    collections_by_query: Vec<CollectionSearchResult>,
}

#[derive(Deserialize)]
struct CollectionSearchResult {
    #[serde(rename = "__typename")]
    kind: String,
    slug: Option<String>,
    address: Option<String>,
    chain: Option<ChainIdent>,
}

#[derive(Deserialize)]
struct ChainIdent {
    identifier: Option<String>,
    #[serde(rename = "networkId")]
    network_id: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EligibilityVariables {
    address: String,
    collection_slug: String,
}

#[derive(Serialize, Deserialize)]
struct CollectionSearchVariables {
    query: String,
}

#[derive(Deserialize)]
struct EligibilityQueryData {
    #[serde(rename = "dropBySlug")]
    drop_by_slug: Option<EligibilityDrop>,
}

#[derive(Deserialize)]
struct EligibilityDrop {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    stages: Vec<StageEligibilityResp>,
}

#[derive(Deserialize)]
struct StageEligibilityResp {
    #[serde(rename = "stageType")]
    stage_type: String,
    #[serde(rename = "stageIndex", deserialize_with = "deserialize_stage_index")]
    stage_index: u32,
    #[serde(rename = "isEligible")]
    is_eligible: Option<bool>,
    #[serde(rename = "eligibleMinterAddress")]
    eligible_minter_address: Option<String>,
    #[serde(rename = "maxTotalMintableByWallet", default)]
    max_total_mintable_by_wallet: Option<serde_json::Value>,
    #[serde(rename = "eligibleMaxTotalMintableByWallet", default)]
    eligible_max_total_mintable_by_wallet: Option<serde_json::Value>,
    #[serde(rename = "startsAt", default)]
    starts_at: Option<serde_json::Value>,
    #[serde(rename = "eligiblePrice", default)]
    eligible_price: Option<EligiblePriceResp>,
}

#[derive(Deserialize)]
struct EligiblePriceResp {
    #[serde(default)]
    usd: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default)]
pub struct StageAssessment {
    pub stage_type: String,
    pub stage_index: u32,
    pub is_eligible: Option<bool>,
    pub eligible_minter: Option<String>,
    pub max_total_mintable_by_wallet: Option<u64>,
    pub eligible_max_total_mintable_by_wallet: Option<u64>,
    /// USD unit price when OpenSea reports it (None = unknown/absent).
    pub price_usd: Option<f64>,
    /// Stage open time in unix ms when reported.
    pub starts_at_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ResolvedCollection {
    pub slug: String,
    pub network_id: u64,
    pub address: String,
    pub chain_identifier: String,
}

/// Calldata + payment target returned by OpenSea `MintActionTimelineQuery`.
#[derive(Clone, Debug)]
pub struct MintActionTx {
    pub to: String,
    pub calldata: String,
    pub value_wei: String,
    pub network_id: u64,
}

/// Ready-to-enqueue plan for one wallet on an OpenSea stage.
#[derive(Clone, Debug, Serialize)]
pub struct OpenSeaMintPlan {
    pub to: String,
    pub calldata: String,
    pub value_wei: String,
    pub value_eth: String,
    pub quantity: i64,
    pub stage_type: String,
    pub stage_index: u32,
    pub slug: String,
    pub network_id: u64,
    pub nft_contract: String,
}

pub struct OpenSeaClient {
    client: reqwest::Client,
    jar: Arc<Jar>,
}

impl OpenSeaClient {
    pub fn new() -> AppResult<Self> {
        let jar = Arc::new(Jar::default());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .cookie_provider(jar.clone())
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Aegis/0.1")
            .build()
            .map_err(|_| os_err("http client"))?;
        Ok(Self { client, jar })
    }

    fn collection_referer(&self, slug: &str) -> AppResult<Url> {
        collection_url(slug)
    }

    pub async fn resolve_collection(
        &self,
        contract: &str,
        expected_network: Option<u64>,
    ) -> AppResult<Option<ResolvedCollection>> {
        let addr: Address = contract
            .parse()
            .map_err(|_| AppError::Invalid("bad contract address".into()))?;
        let query = addr.to_checksum(None);
        let variables = CollectionSearchVariables { query };
        let body = GraphQlRequest {
            operation_name: "MintCollectionSearch",
            query: COLLECTION_SEARCH_QUERY,
            variables: &variables,
        };
        let resp = self
            .client
            .post(GRAPHQL_URL)
            .header(ACCEPT, "application/json")
            .header(ORIGIN, origin())
            .header(REFERER, origin())
            .header("x-app-id", APP_ID)
            .json(&body)
            .send()
            .await
            .map_err(|_| os_err("collection search transport"))?;
        check_status(resp.status().as_u16())?;
        let text = limited_text(resp, GQL_LIMIT).await?;
        let envelope: GraphQlEnvelope<CollectionSearchData> =
            serde_json::from_str(&text).map_err(|_| os_err("collection search decode"))?;
        let data = decode_envelope(envelope)?;
        let mut candidates = Vec::new();
        for r in data.collections_by_query {
            if r.kind != "Collection" {
                continue;
            }
            let Some(slug) = r.slug.clone() else {
                continue;
            };
            if validate_slug(&slug).is_err() {
                continue;
            }
            let Some(addr_s) = r.address.as_deref() else {
                continue;
            };
            let Ok(ra) = addr_s.parse::<Address>() else {
                continue;
            };
            if ra != addr {
                continue;
            }
            let network = r
                .chain
                .as_ref()
                .and_then(|c| c.network_id.as_ref())
                .and_then(parse_json_number_u64);
            let chain_identifier = r
                .chain
                .as_ref()
                .and_then(|c| c.identifier.clone())
                .unwrap_or_default();
            candidates.push((slug, network, addr_s.to_string(), chain_identifier));
        }
        if candidates.is_empty() {
            return Ok(None);
        }
        let picked = if let Some(exp) = expected_network {
            let exact: Vec<_> = candidates
                .iter()
                .filter(|(_, n, _, _)| *n == Some(exp))
                .cloned()
                .collect();
            if exact.len() == 1 {
                exact.into_iter().next()
            } else if candidates.len() == 1 {
                candidates.into_iter().next()
            } else if exact.is_empty() {
                return Err(os_err("collection on different OpenSea network"));
            } else {
                return Err(os_err("ambiguous collection"));
            }
        } else if candidates.len() == 1 {
            candidates.into_iter().next()
        } else {
            return Err(os_err("ambiguous collection"));
        };
        let (slug, network, address, chain_identifier) = picked.unwrap();
        Ok(Some(ResolvedCollection {
            slug,
            network_id: network.unwrap_or(expected_network.unwrap_or(0)),
            address,
            chain_identifier,
        }))
    }

    fn set_hint_cookie(&self, wallet: &Address) -> AppResult<()> {
        let site = parse_url(SITE_URL)?;
        let domain = site.host_str().ok_or_else(|| os_err("site host"))?;
        let cookie = format!(
            "{CONNECTED_ACCOUNT_HINT_COOKIE}={wallet:#x}; Domain={domain}; Path=/; Secure; SameSite=Lax"
        );
        self.jar.add_cookie_str(&cookie, &site);
        Ok(())
    }

    async fn request_nonce(&self, referer: &Url) -> AppResult<String> {
        let url = parse_url(SITE_URL)?
            .join("/__api/auth/siwe/nonce")
            .map_err(|_| os_err("nonce url"))?;
        let resp = self
            .client
            .post(url)
            .header(ORIGIN, origin())
            .header(REFERER, referer.as_str())
            .send()
            .await
            .map_err(|_| os_err("nonce transport"))?;
        if !resp.status().is_success() {
            return Err(os_err(format!("nonce http {}", resp.status().as_u16())));
        }
        let text = limited_text(resp, NONCE_LIMIT).await?;
        let parsed: NonceResponse = serde_json::from_str(&text).map_err(|_| os_err("nonce decode"))?;
        let nonce = parsed.nonce;
        if !(8..=256).contains(&nonce.len())
            || !nonce.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(os_err("invalid nonce"));
        }
        Ok(nonce)
    }

    pub async fn authenticate(
        &self,
        wallet_id: i64,
        wallet: &Address,
        chain_id: u64,
        slug: &str,
    ) -> AppResult<()> {
        if chain_id == 0 {
            return Err(AppError::Invalid("chain id required for SIWE".into()));
        }
        let (nonce_ct, ct) = crate::wallet_store::key_material(wallet_id)?;
        let signer = wallet::load_signer(&nonce_ct, &ct)?;
        if signer.address() != *wallet {
            return Err(os_err("signer address mismatch"));
        }
        validate_slug(slug)?;
        let referer = self.collection_referer(slug)?;
        self.set_hint_cookie(wallet)?;
        let nonce = self.request_nonce(&referer).await?;
        let issued_at =
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let domain = "opensea.io";
        let address = wallet.to_checksum(None);
        let uri = referer.as_str();
        let message = create_siwe_message(domain, &address, uri, chain_id, &nonce, &issued_at);
        let sig = signer
            .sign_message_sync(message.as_bytes())
            .map_err(|e| AppError::Crypto(format!("siwe sign: {e}")))?;
        let signature = format!("0x{}", hex::encode(sig.as_bytes()));
        let body = VerifyRequest {
            message: ParsedSiweMessage {
                domain,
                address: &address,
                statement: SIWE_STATEMENT,
                uri,
                version: "1",
                chain_id: chain_id.to_string(),
                nonce: &nonce,
                issued_at: &issued_at,
                account_type: "Ethereum",
            },
            signature: &signature,
            chain_arch: "EVM",
        };
        let url = parse_url(SITE_URL)?
            .join("/__api/auth/siwe/verify")
            .map_err(|_| os_err("verify url"))?;
        let resp = self
            .client
            .post(url)
            .header(ORIGIN, origin())
            .header(REFERER, referer.as_str())
            .json(&body)
            .send()
            .await
            .map_err(|_| os_err("verify transport"))?;
        if !resp.status().is_success() {
            return Err(os_err(format!("verify http {}", resp.status().as_u16())));
        }
        let text = limited_text(resp, AUTH_LIMIT).await?;
        let parsed: AuthenticationResponse =
            serde_json::from_str(&text).map_err(|_| os_err("verify decode"))?;
        let auth_addr: Address = parsed
            .user
            .address
            .parse()
            .map_err(|_| os_err("verify address"))?;
        if auth_addr != *wallet {
            return Err(os_err("session wallet mismatch"));
        }
        Ok(())
    }

    pub async fn fetch_stages(&self, slug: &str, wallet: &Address) -> AppResult<Vec<StageAssessment>> {
        validate_slug(slug)?;
        let referer = self.collection_referer(slug)?;
        let address = format!("{wallet:#x}");
        let variables = EligibilityVariables {
            address,
            collection_slug: slug.to_string(),
        };

        // Prefer persisted-query GET (faster, matches production capture).
        let mut url = parse_url(GRAPHQL_URL)?;
        {
            let vars = serde_json::to_string(&variables).map_err(|_| os_err("vars"))?;
            let extensions = serde_json::json!({
                "persistedQuery": {
                    "sha256Hash": ELIGIBILITY_PERSISTED_QUERY_HASH,
                    "version": 1
                }
            })
            .to_string();
            url.query_pairs_mut()
                .append_pair("app_id", APP_ID)
                .append_pair("operationName", "DropEligibilityQuery")
                .append_pair("variables", &vars)
                .append_pair("extensions", &extensions);
        }

        let resp = self
            .client
            .get(url)
            .header(ACCEPT, "application/json")
            .header(ORIGIN, origin())
            .header(REFERER, referer.as_str())
            .send()
            .await
            .map_err(|_| os_err("eligibility transport"))?;
        check_status(resp.status().as_u16())?;
        let text = limited_text(resp, GQL_LIMIT).await?;

        if let Ok(env) = serde_json::from_str::<GraphQlEnvelope<EligibilityQueryData>>(&text) {
            let persisted_retry = env
                .errors
                .iter()
                .any(|e| {
                    let m = e.message.to_ascii_lowercase();
                    m.contains("persistedquery")
                });
            if !persisted_retry {
                let data = decode_envelope(env)?;
                return Ok(map_stages(data));
            }
        }

        // Fallback: full query POST.
        let body = GraphQlRequest {
            operation_name: "DropEligibilityQuery",
            query: ELIGIBILITY_QUERY,
            variables: &variables,
        };
        let resp = self
            .client
            .post(GRAPHQL_URL)
            .header(ACCEPT, "application/json")
            .header(ORIGIN, origin())
            .header(REFERER, referer.as_str())
            .header("x-app-id", APP_ID)
            .json(&body)
            .send()
            .await
            .map_err(|_| os_err("eligibility post transport"))?;
        check_status(resp.status().as_u16())?;
        let text = limited_text(resp, GQL_LIMIT).await?;
        let env: GraphQlEnvelope<EligibilityQueryData> =
            serde_json::from_str(&text).map_err(|_| os_err("eligibility decode"))?;
        let data = decode_envelope(env)?;
        Ok(map_stages(data))
    }

    /// Fetch OpenSea `MintActionTimelineQuery` calldata for an authenticated session.
    pub async fn fetch_mint_action(
        &self,
        resolved: &ResolvedCollection,
        wallet: &Address,
        quantity: u64,
        token_id: &str,
    ) -> AppResult<MintActionTx> {
        if quantity == 0
            || token_id.is_empty()
            || !token_id.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(AppError::Invalid("bad mint quantity/token id".into()));
        }
        if resolved.chain_identifier.is_empty() {
            return Err(os_err("missing collection chain identifier"));
        }
        validate_slug(&resolved.slug)?;
        let referer = self.collection_referer(&resolved.slug)?;
        let drop_address: Address = resolved
            .address
            .parse()
            .map_err(|_| os_err("bad drop address"))?;
        let native = Address::ZERO.to_checksum(None);
        let quantity_s = quantity.to_string();
        let variables = MintActionVariables {
            address: wallet.to_checksum(None),
            from_assets: vec![AssetQuantityInput {
                asset: AssetInput {
                    contract_address: native,
                    chain: resolved.chain_identifier.clone(),
                    token_id: None,
                },
                quantity: None,
            }],
            to_assets: vec![AssetQuantityInput {
                asset: AssetInput {
                    contract_address: drop_address.to_checksum(None),
                    chain: resolved.chain_identifier.clone(),
                    token_id: Some(token_id.to_string()),
                },
                quantity: Some(quantity_s),
            }],
            recipient: None,
        };
        let body = GraphQlRequest {
            operation_name: "MintActionTimelineQuery",
            query: MINT_ACTION_QUERY,
            variables: &variables,
        };
        let resp = self
            .client
            .post(GRAPHQL_URL)
            .header(ACCEPT, "application/json")
            .header(ORIGIN, origin())
            .header(REFERER, referer.as_str())
            .header("x-app-id", APP_ID)
            .json(&body)
            .send()
            .await
            .map_err(|_| os_err("mint action transport"))?;
        check_status(resp.status().as_u16())?;
        let text = limited_text(resp, GQL_LIMIT).await?;
        let env: GraphQlEnvelope<MintActionQueryData> =
            serde_json::from_str(&text).map_err(|_| os_err("mint action decode"))?;
        let data = decode_envelope(env)?;
        decode_mint_action(data)
    }

    /// Full per-wallet plan: SIWE → stages → pick eligible → MintAction calldata.
    pub async fn plan_wallet_mint(
        &self,
        wallet_id: i64,
        wallet: &Address,
        resolved: &ResolvedCollection,
        expected_network: u64,
        quantity: u64,
        token_id: &str,
    ) -> AppResult<OpenSeaMintPlan> {
        let network = if resolved.network_id != 0 {
            resolved.network_id
        } else {
            expected_network
        };
        if network == 0 {
            return Err(os_err("unknown collection network"));
        }
        self.authenticate(wallet_id, wallet, network, &resolved.slug)
            .await?;
        let stages = self.fetch_stages(&resolved.slug, wallet).await?;

        // Schedule guard: never ask OpenSea for calldata while no eligible
        // stage is actually open. DropNotMintingError comes from that exact
        // case, so detect it here and return an actionable message instead.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let eligible_stages: Vec<&StageAssessment> =
            stages.iter().filter(|s| s.is_eligible.unwrap_or(false)).collect();
        let live_stage = eligible_stages
            .iter()
            .find(|s| s.starts_at_ms.map(|t| t <= now_ms).unwrap_or(true));
        if live_stage.is_none() {
            // Everything eligible is scheduled for later (or unknown).
            let next = eligible_stages
                .iter()
                .filter_map(|s| s.starts_at_ms)
                .min();
            let hint = match next {
                Some(ts) => {
                    chrono::DateTime::from_timestamp_millis(ts)
                        .map(|d| format!("drop not live yet — opens {}", d.format("%Y-%m-%d %H:%M UTC")))
                        .unwrap_or_else(|| "drop not live yet".into())
                }
                None => "drop not live yet (no schedule reported)".into(),
            };
            return Err(os_err(hint));
        }

        let stage = pick_eligible_stage(&stages)
            .ok_or_else(|| os_err("no eligible OpenSea stage for this wallet"))?
            .clone();
        let action = self
            .fetch_mint_action(resolved, wallet, quantity, token_id)
            .await?;
        validate_mint_action(
            &action,
            resolved,
            wallet,
            &stage.stage_type,
            stage.stage_index,
            quantity,
            network,
        )?;
        let value_u256 = alloy::primitives::U256::from_str_radix(
            action.value_wei.trim_start_matches("0x"),
            if action.value_wei.starts_with("0x") { 16 } else { 10 },
        )
        .map_err(|_| os_err("bad mint action value"))?;
        Ok(OpenSeaMintPlan {
            to: action.to,
            calldata: action.calldata,
            value_wei: action.value_wei,
            value_eth: crate::mint::seadrop::format_eth(value_u256),
            quantity: i64::try_from(quantity).unwrap_or(1),
            stage_type: stage.stage_type,
            stage_index: stage.stage_index,
            slug: resolved.slug.clone(),
            network_id: network,
            nft_contract: resolved.address.clone(),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MintActionVariables {
    address: String,
    from_assets: Vec<AssetQuantityInput>,
    to_assets: Vec<AssetQuantityInput>,
    recipient: Option<String>,
}

#[derive(Serialize)]
struct AssetQuantityInput {
    asset: AssetInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    quantity: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AssetInput {
    contract_address: String,
    chain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_id: Option<String>,
}

#[derive(Deserialize)]
struct MintActionQueryData {
    swap: Option<SwapResponse>,
}

#[derive(Deserialize)]
struct SwapResponse {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    actions: Vec<ActionResponse>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    errors: Vec<ActionErrorResponse>,
}

#[derive(Deserialize)]
struct ActionResponse {
    #[serde(rename = "__typename")]
    kind: String,
    #[serde(rename = "transactionSubmissionData")]
    transaction_submission_data: Option<TransactionSubmissionData>,
}

#[derive(Deserialize)]
struct ActionErrorResponse {
    #[serde(rename = "__typename")]
    kind: String,
}

#[derive(Deserialize)]
struct TransactionSubmissionData {
    to: String,
    data: String,
    value: Option<serde_json::Value>,
    chain: TransactionChain,
}

#[derive(Deserialize)]
struct TransactionChain {
    #[serde(rename = "networkId")]
    network_id: serde_json::Value,
    #[serde(default)]
    identifier: Option<String>,
}

fn decode_mint_action(data: MintActionQueryData) -> AppResult<MintActionTx> {
    let swap = data.swap.ok_or_else(|| os_err("missing swap"))?;
    if !swap.errors.is_empty() {
        let kinds: Vec<_> = swap.errors.iter().map(|e| e.kind.as_str()).collect();
        return Err(os_err(format!("mint action rejected: {}", kinds.join(", "))));
    }
    let mut kinds = Vec::new();
    let mut txs = Vec::new();
    for a in swap.actions {
        kinds.push(a.kind);
        if let Some(t) = a.transaction_submission_data {
            txs.push(t);
        }
    }
    if kinds.as_slice() != ["MintAction"] {
        return Err(os_err(format!("unexpected action sequence: {kinds:?}")));
    }
    if txs.len() != 1 {
        return Err(os_err("expected exactly one mint transaction"));
    }
    let tx = txs.into_iter().next().unwrap();
    let to: Address = tx.to.parse().map_err(|_| os_err("bad mint action to"))?;
    if to == Address::ZERO {
        return Err(os_err("zero mint action target"));
    }
    let network_id = parse_json_number_u64(&tx.chain.network_id).unwrap_or(0);
    if network_id == 0 {
        return Err(os_err("missing mint action network id"));
    }
    if tx.chain.identifier.as_deref().is_some_and(|s| s.is_empty()) {
        return Err(os_err("missing mint action chain identifier"));
    }
    let calldata = tx.data;
    if !calldata.starts_with("0x") || calldata.len() < 10 {
        return Err(os_err("mint action calldata too short"));
    }
    let value_wei = tx
        .value
        .as_ref()
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| "0".into());
    Ok(MintActionTx {
        to: to.to_checksum(None),
        calldata,
        value_wei,
        network_id,
    })
}

fn validate_mint_action(
    action: &MintActionTx,
    resolved: &ResolvedCollection,
    wallet: &Address,
    stage_type: &str,
    stage_index: u32,
    quantity: u64,
    expected_network: u64,
) -> AppResult<()> {
    if action.network_id != expected_network {
        return Err(os_err("mint action network mismatch"));
    }
    if action.to.is_empty() {
        return Err(os_err("empty mint action to"));
    }
    validate_stage_calldata(
        &action.calldata,
        resolved,
        wallet,
        stage_type,
        stage_index,
        quantity,
    )
}

/// Erc721SeaDropV1 calldata checks (port of osnm-z `validate_stage_calldata`).
fn validate_stage_calldata(
    calldata_hex: &str,
    resolved: &ResolvedCollection,
    wallet: &Address,
    stage_type: &str,
    stage_index: u32,
    quantity: u64,
) -> AppResult<()> {
    let raw = calldata_hex.trim_start_matches("0x");
    if !raw.len().is_multiple_of(2) {
        return Err(os_err("odd calldata length"));
    }
    let bytes = hex::decode(raw).map_err(|_| os_err("calldata not hex"))?;
    if bytes.len() < 4 + 9 * 32 {
        return Err(os_err("calldata too short for stage validation"));
    }
    let expected_selector: [u8; 4] = match stage_type {
        "PUBLIC_SALE" => [0x16, 0x1a, 0xc2, 0x1f],
        "SIGNED_PRESALE" => [0x4b, 0x61, 0xcd, 0x6f],
        "MERKLE_PRESALE" => [0x43, 0x00, 0xa4, 0xe6],
        other => return Err(os_err(format!("unknown stage type {other}"))),
    };
    if bytes[..4] != expected_selector {
        return Err(os_err("mint selector mismatch"));
    }
    let nft_contract = decode_abi_address(&bytes, 0)?;
    let expected_nft: Address = resolved
        .address
        .parse()
        .map_err(|_| os_err("bad resolved address"))?;
    if nft_contract != expected_nft {
        return Err(os_err("NFT contract mismatch"));
    }
    let minter_if_not_payer = decode_abi_address(&bytes, 2)?;
    if minter_if_not_payer != Address::ZERO && minter_if_not_payer != *wallet {
        return Err(os_err("minter mismatch"));
    }
    let qty = decode_abi_u64(&bytes, 3)?;
    if qty != quantity {
        return Err(os_err("mint quantity mismatch"));
    }
    if stage_type == "PUBLIC_SALE" {
        if stage_index != 0 {
            return Err(os_err("public sale stage index must be 0"));
        }
    } else if decode_abi_u64(&bytes, 8)? != u64::from(stage_index) {
        return Err(os_err("mint stage index mismatch"));
    }
    Ok(())
}

fn abi_word(bytes: &[u8], word_index: usize) -> AppResult<&[u8]> {
    let start = 4 + word_index * 32;
    let end = start + 32;
    bytes
        .get(start..end)
        .ok_or_else(|| os_err("calldata word out of bounds"))
}

fn decode_abi_address(bytes: &[u8], word_index: usize) -> AppResult<Address> {
    let word = abi_word(bytes, word_index)?;
    if word[..12] != [0_u8; 12] {
        return Err(os_err("address ABI word is not canonical"));
    }
    Ok(Address::from_slice(&word[12..]))
}

fn decode_abi_u64(bytes: &[u8], word_index: usize) -> AppResult<u64> {
    let word = abi_word(bytes, word_index)?;
    if word[..24] != [0_u8; 24] {
        return Err(os_err("uint64 ABI word is not canonical"));
    }
    let mut buf = [0_u8; 8];
    buf.copy_from_slice(&word[24..]);
    Ok(u64::from_be_bytes(buf))
}

fn map_stages(data: EligibilityQueryData) -> Vec<StageAssessment> {
    let Some(drop) = data.drop_by_slug else {
        return Vec::new();
    };
    drop.stages
        .into_iter()
        .map(|s| StageAssessment {
            stage_type: s.stage_type,
            stage_index: s.stage_index,
            is_eligible: s.is_eligible,
            eligible_minter: s.eligible_minter_address,
            max_total_mintable_by_wallet: s
                .max_total_mintable_by_wallet
                .as_ref()
                .and_then(parse_json_number_u64),
            eligible_max_total_mintable_by_wallet: s
                .eligible_max_total_mintable_by_wallet
                .as_ref()
                .and_then(parse_json_number_u64),
            price_usd: s
                .eligible_price
                .as_ref()
                .and_then(|p| p.usd.as_ref())
                .and_then(parse_json_f64),
            starts_at_ms: s.starts_at.as_ref().and_then(parse_starts_at_ms),
        })
        .collect()
}

fn decode_envelope<T>(env: GraphQlEnvelope<T>) -> AppResult<T> {
    if !env.errors.is_empty() {
        let msgs: Vec<_> = env.errors.iter().map(|e| e.message.clone()).collect();
        let joined = msgs.join("; ");
        let lower = joined.to_ascii_lowercase();
        if lower.contains("not authenticated")
            || lower.contains("authentication")
            || lower.contains("unauthenticated")
            || lower.contains("unauthorized")
        {
            return Err(os_err("authentication required"));
        }
        if lower.contains("rate limit") || lower.contains("too many requests") {
            return Err(os_err("rate limited"));
        }
        return Err(os_err(joined));
    }
    env.data.ok_or_else(|| os_err("missing data"))
}

fn check_status(status: u16) -> AppResult<()> {
    match status {
        401 => Err(os_err("authentication required")),
        429 => Err(os_err("rate limited")),
        s if s >= 400 => Err(os_err(format!("http {s}"))),
        _ => Ok(()),
    }
}

async fn limited_text(resp: reqwest::Response, limit: usize) -> AppResult<String> {
    if resp
        .content_length()
        .is_some_and(|l| l > limit as u64)
    {
        return Err(os_err("response too large"));
    }
    let text = resp
        .text()
        .await
        .map_err(|_| os_err("response body"))?;
    if text.len() > limit {
        return Err(os_err("response too large"));
    }
    Ok(text)
}

/// Stage assessment (osnm-z port): private stages with isEligible=true win (FCFS/presale);
/// PUBLIC_SALE eligible unless explicitly false.
pub fn assess_stages(stages: &[StageAssessment]) -> (bool, String) {
    if stages.is_empty() {
        return (false, "OpenSea: no drop stages".into());
    }
    if let Some(s) = pick_eligible_stage(stages) {
        let max = s
            .eligible_max_total_mintable_by_wallet
            .or(s.max_total_mintable_by_wallet);
        let max_part = max.map(|m| format!(", max {m}/wallet")).unwrap_or_default();
        if s.stage_type == "PUBLIC_SALE" {
            return (true, format!("eligible via OpenSea (PUBLIC_SALE){max_part}"));
        }
        let via = if s.eligible_minter.is_some() {
            " via linked wallet"
        } else {
            ""
        };
        return (
            true,
            format!("eligible via OpenSea ({}){via}{max_part}", s.stage_type),
        );
    }
    let private_known = stages
        .iter()
        .filter(|s| s.stage_type != "PUBLIC_SALE")
        .any(|s| s.is_eligible.is_some());
    if private_known {
        return (false, "OpenSea: no eligible stage".into());
    }
    (false, "OpenSea: not eligible".into())
}

/// Best stage to mint: private eligible first (FCFS/presale), else open PUBLIC_SALE.
/// Among eligible private stages prefer paid (FCFS with price) over free (GTD),
/// then prefer a stage that has already opened (or whose start is unknown).
pub fn pick_eligible_stage(stages: &[StageAssessment]) -> Option<&StageAssessment> {
    let private: Vec<&StageAssessment> = stages
        .iter()
        .filter(|s| s.stage_type != "PUBLIC_SALE" && s.is_eligible == Some(true))
        .collect();
    if !private.is_empty() {
        let is_paid = |s: &&StageAssessment| s.price_usd.map(|p| p > 0.0).unwrap_or(false);
        if let Some(paid) = private.iter().find(|s| is_paid(s)) {
            return Some(paid);
        }
        let now = crate::db::now_ms();
        let open_or_unknown = private
            .iter()
            .find(|s| s.starts_at_ms.map(|t| t <= now).unwrap_or(true));
        return open_or_unknown.or(private.first()).copied();
    }
    stages
        .iter()
        .find(|s| s.stage_type == "PUBLIC_SALE" && s.is_eligible != Some(false))
}

/// Resolve collection once, then plan a single-wallet OpenSea stage mint.
pub async fn plan_mint(
    wallet_id: i64,
    wallet_addr: &str,
    collection: &str,
    expected_network: u64,
    quantity: u64,
    token_id: Option<&str>,
) -> AppResult<OpenSeaMintPlan> {
    if !crate::vault::is_unlocked().unwrap_or(false) {
        return Err(AppError::VaultLocked);
    }
    let wallet: Address = wallet_addr
        .parse()
        .map_err(|_| AppError::Invalid("bad wallet address".into()))?;
    let client = OpenSeaClient::new()?;
    let resolved = client
        .resolve_collection(collection, Some(expected_network))
        .await?
        .ok_or_else(|| os_err("collection not on OpenSea"))?;
    client
        .plan_wallet_mint(
            wallet_id,
            &wallet,
            &resolved,
            expected_network,
            quantity,
            token_id.unwrap_or("0"),
        )
        .await
}

/// Per-stage result for a single wallet, returned by the matrix check.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletStageResult {
    pub stage_type: String,
    pub eligible: bool,
    pub max_quantity: Option<u64>,
    pub price_usd: Option<f64>,
    /// Stage open time (unix ms). None = unknown / not reported.
    pub starts_at_ms: Option<i64>,
    /// Computed from starts_at_ms vs current time: "live", "not_started", or "unknown".
    pub live_status: String,
    /// Human-readable schedule hint: "Opens Sep 28 12:00 UTC" / "Live now" / "—".
    pub schedule_hint: String,
}

/// Check one wallet against all stages of a collection.
/// Returns `(slug, per-stage results)` or `None` if not on OpenSea.
/// Requires vault unlocked (SIWE signing).
pub async fn check_wallet_stages(
    wallet_id: i64,
    wallet_addr: &str,
    collection: &str,
    expected_network: u64,
) -> AppResult<Option<(String, Vec<WalletStageResult>)>> {
    if !crate::vault::is_unlocked().unwrap_or(false) {
        return Err(AppError::VaultLocked);
    }
    let wallet: Address = wallet_addr
        .parse()
        .map_err(|_| AppError::Invalid("bad wallet address".into()))?;
    let client = OpenSeaClient::new()?;
    let Some(resolved) = client
        .resolve_collection(collection, Some(expected_network))
        .await?
    else {
        return Ok(None); // collection not on OpenSea
    };
    let network = if resolved.network_id != 0 {
        resolved.network_id
    } else {
        expected_network
    };
    if network == 0 {
        return Err(os_err("unknown collection network"));
    }
    client
        .authenticate(wallet_id, &wallet, network, &resolved.slug)
        .await?;
    let stages = client.fetch_stages(&resolved.slug, &wallet).await?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let results: Vec<WalletStageResult> = stages
        .into_iter()
        .map(|s| {
            let starts_at = s.starts_at_ms;
            let (live_status, schedule_hint) = match starts_at {
                Some(ts) if ts > now_ms => {
                    ("not_started".to_string(), format_hint_future(ts))
                }
                Some(_) => ("live".to_string(), "Live now".to_string()),
                None => ("unknown".to_string(), "—".to_string()),
            };
            WalletStageResult {
                stage_type: s.stage_type,
                eligible: s.is_eligible.unwrap_or(false),
                max_quantity: s
                    .eligible_max_total_mintable_by_wallet
                    .or(s.max_total_mintable_by_wallet),
                price_usd: s.price_usd,
                starts_at_ms: starts_at,
                live_status,
                schedule_hint,
            }
        })
        .collect();
    Ok(Some((resolved.slug, results)))
}

/// Full check for one wallet. `Ok(None)` = collection not on OpenSea (caller falls back).
/// Requires vault unlocked (SIWE signing).
pub async fn check_wallet(
    wallet_id: i64,
    wallet_addr: &str,
    collection: &str,
    expected_network: u64,
) -> AppResult<Option<(bool, String)>> {
    if !crate::vault::is_unlocked().unwrap_or(false) {
        return Err(AppError::VaultLocked);
    }
    let wallet: Address = wallet_addr
        .parse()
        .map_err(|_| AppError::Invalid("bad wallet address".into()))?;
    let client = OpenSeaClient::new()?;
    let Some(resolved) = client
        .resolve_collection(collection, Some(expected_network))
        .await?
    else {
        return Ok(None);
    };
    let network = if resolved.network_id != 0 {
        resolved.network_id
    } else {
        expected_network
    };
    if network == 0 {
        return Err(os_err("unknown collection network"));
    }
    client
        .authenticate(wallet_id, &wallet, network, &resolved.slug)
        .await?;
    let stages = client.fetch_stages(&resolved.slug, &wallet).await?;
    let (eligible, detail) = assess_stages(&stages);
    Ok(Some((eligible, detail)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn siwe_message_shape() {
        let msg = create_siwe_message(
            "opensea.io",
            "0xabc",
            "https://opensea.io/collection/x/overview",
            4663,
            "nonce123",
            "2026-09-24T00:00:00Z",
        );
        assert!(msg.starts_with("opensea.io wants you to sign in with your Ethereum account:\n0xabc\n\n"));
        assert!(msg.contains("Chain ID: 4663\nNonce: nonce123\nIssued At: 2026-09-24T00:00:00Z"));
        assert!(msg.contains("URI: https://opensea.io/collection/x/overview\nVersion: 1"));
    }

    #[test]
    fn slug_ok() {
        assert!(validate_slug("boredapeyachtclub").is_ok());
        assert!(validate_slug("bad/slug").is_err());
        assert!(validate_slug("").is_err());
    }

    #[test]
    fn assess_fcfs_eligible() {
        let stages = vec![
            StageAssessment {
                stage_type: "SIGNED_PRESALE".into(),
                stage_index: 2,
                is_eligible: Some(true),
                eligible_minter: Some("0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5".into()),
                max_total_mintable_by_wallet: Some(3),
                eligible_max_total_mintable_by_wallet: Some(3),
                ..Default::default()
            },
            StageAssessment {
                stage_type: "PUBLIC_SALE".into(),
                stage_index: 0,
                is_eligible: Some(false),
                eligible_minter: None,
                max_total_mintable_by_wallet: None,
                eligible_max_total_mintable_by_wallet: None,
                ..Default::default()
            },
        ];
        let (ok, detail) = assess_stages(&stages);
        assert!(ok);
        assert!(detail.contains("SIGNED_PRESALE"));
        let picked = pick_eligible_stage(&stages).unwrap();
        assert_eq!(picked.stage_type, "SIGNED_PRESALE");
        assert_eq!(picked.stage_index, 2);
    }

    #[test]
    fn assess_public_sale() {
        let stages = vec![StageAssessment {
            stage_type: "PUBLIC_SALE".into(),
            stage_index: 0,
            is_eligible: Some(true),
            eligible_minter: None,
            max_total_mintable_by_wallet: None,
            eligible_max_total_mintable_by_wallet: None,
            ..Default::default()
        }];
        let (ok, detail) = assess_stages(&stages);
        assert!(ok);
        assert!(detail.contains("PUBLIC_SALE"));
    }

    #[test]
    fn assess_private_ineligible() {
        let stages = vec![StageAssessment {
            stage_type: "MERKLE_PRESALE".into(),
            stage_index: 1,
            is_eligible: Some(false),
            eligible_minter: None,
            max_total_mintable_by_wallet: None,
            eligible_max_total_mintable_by_wallet: None,
            ..Default::default()
        }];
        let (ok, _) = assess_stages(&stages);
        assert!(!ok);
    }

    #[test]
    fn prefers_paid_fcfs_over_free_gtd() {
        let now_ms = crate::db::now_ms();
        let stages = vec![
            StageAssessment {
                stage_type: "SIGNED_PRESALE".into(),
                stage_index: 1,
                is_eligible: Some(true),
                price_usd: Some(0.0), // free GTD — first in list
                starts_at_ms: Some(now_ms - 60_000),
                ..Default::default()
            },
            StageAssessment {
                stage_type: "FCFS_ALLOWLIST".into(),
                stage_index: 2,
                is_eligible: Some(true),
                price_usd: Some(1.02), // paid FCFS — should win
                starts_at_ms: Some(now_ms + 30_000), // not open yet
                ..Default::default()
            },
        ];
        let picked = pick_eligible_stage(&stages).unwrap();
        assert_eq!(picked.stage_type, "FCFS_ALLOWLIST");
        assert_eq!(picked.stage_index, 2);
    }

    #[test]
    fn assess_empty() {
        let (ok, detail) = assess_stages(&[]);
        assert!(!ok);
        assert!(detail.contains("no drop stages"));
    }

    #[test]
    fn validates_signed_presale_calldata() {
        let nft = "0xe1dd28bb9c61dac72f7a47f0a2c712eb4976ec2f";
        let wallet: Address = "0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5"
            .parse()
            .unwrap();
        let resolved = ResolvedCollection {
            slug: "x".into(),
            network_id: 4663,
            address: nft.into(),
            chain_identifier: "robinhood".into(),
        };
        // selector mintSigned-ish + words: nft, ?, minter=0, qty=1, … stageIndex@8
        let mut body = String::from("4b61cd6f");
        let nft_word = {
            let a: Address = nft.parse().unwrap();
            format!("{:0>64}", hex::encode(a.as_slice()))
        };
        body.push_str(&nft_word);
        body.push_str(&"0".repeat(64)); // word1
        body.push_str(&"0".repeat(64)); // minterIfNotPayer = zero
        body.push_str(&format!("{:0>64}", "1")); // qty
        for _ in 4..8 {
            body.push_str(&"0".repeat(64));
        }
        body.push_str(&format!("{:0>64}", "2")); // stage index 8
        let cd = format!("0x{body}");
        validate_stage_calldata(&cd, &resolved, &wallet, "SIGNED_PRESALE", 2, 1).unwrap();
        assert!(validate_stage_calldata(&cd, &resolved, &wallet, "SIGNED_PRESALE", 3, 1).is_err());
        assert!(validate_stage_calldata(&cd, &resolved, &wallet, "SIGNED_PRESALE", 2, 2).is_err());
        assert!(validate_stage_calldata(&cd, &resolved, &wallet, "PUBLIC_SALE", 2, 1).is_err());
    }
}
