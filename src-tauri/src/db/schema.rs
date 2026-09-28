pub const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS meta(
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wallet_groups(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS wallets(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  label TEXT NOT NULL,
  address TEXT NOT NULL UNIQUE,
  enc_privkey BLOB NOT NULL,
  nonce BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  group_id INTEGER REFERENCES wallet_groups(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS chains(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  chain_id INTEGER NOT NULL UNIQUE,
  rpc_url TEXT NOT NULL,
  symbol TEXT NOT NULL,
  explorer TEXT,
  enabled INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS api_keys(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  provider TEXT NOT NULL UNIQUE,
  key_enc BLOB NOT NULL,
  nonce BLOB NOT NULL,
  base_url TEXT
);

CREATE TABLE IF NOT EXISTS mint_tasks(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  chain_id INTEGER NOT NULL,
  contract TEXT NOT NULL,
  quantity INTEGER NOT NULL DEFAULT 1,
  value_wei TEXT,
  calldata TEXT,
  status TEXT NOT NULL DEFAULT 'pending',
  tx_hash TEXT,
  error TEXT,
  wallet_id INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  function_name TEXT,
  is_hex INTEGER NOT NULL DEFAULT 0,
  parameters TEXT,
  rpc_endpoints TEXT,
  flashbots INTEGER NOT NULL DEFAULT 0,
  gas_limit TEXT,
  max_fee_gwei TEXT,
  priority_fee_gwei TEXT,
  nonce_override TEXT,
  scheduled_at INTEGER,
  delay_ms INTEGER NOT NULL DEFAULT 0,
  mode TEXT NOT NULL DEFAULT 'execute',
  poll_attempts INTEGER NOT NULL DEFAULT 0,
  auto_retries INTEGER NOT NULL DEFAULT 0,
  opensea_ref TEXT
);

CREATE TABLE IF NOT EXISTS eligibility_checks(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  wallet_id INTEGER,
  collection TEXT NOT NULL,
  result INTEGER NOT NULL,
  detail TEXT,
  checked_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS eligibility_matrix(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  wallet_id INTEGER NOT NULL,
  collection TEXT NOT NULL,
  stage_name TEXT NOT NULL,
  eligible INTEGER NOT NULL DEFAULT 0,
  max_quantity INTEGER,
  detail TEXT,
  checked_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_elig_matrix_lookup
  ON eligibility_matrix(collection, wallet_id, checked_at DESC);

CREATE TABLE IF NOT EXISTS activity(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  summary TEXT NOT NULL,
  payload TEXT,
  ok INTEGER NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS nft_cache(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  wallet_id INTEGER,
  chain_id INTEGER NOT NULL,
  contract TEXT NOT NULL,
  token_id TEXT NOT NULL,
  uri TEXT,
  image TEXT,
  name TEXT,
  opensea_url TEXT,
  fetched_at INTEGER NOT NULL,
  UNIQUE(wallet_id, chain_id, contract, token_id)
);

CREATE TABLE IF NOT EXISTS pnl_scans(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  wallet_id INTEGER NOT NULL,
  chain_id INTEGER NOT NULL,
  payload TEXT NOT NULL,
  status TEXT NOT NULL,
  scanned_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS collection_pnl_scans(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  contract TEXT NOT NULL,
  chain_id INTEGER NOT NULL,
  payload TEXT NOT NULL,
  scanned_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS fund_jobs(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  mode TEXT NOT NULL,
  amount_mode TEXT NOT NULL,
  asset TEXT NOT NULL,
  chain_id INTEGER NOT NULL,
  amount_wei TEXT NOT NULL,
  anchor_wallet_id INTEGER NOT NULL,
  peer_wallet_ids TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'running',
  total_count INTEGER NOT NULL DEFAULT 0,
  success_count INTEGER NOT NULL DEFAULT 0,
  failed_count INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS fund_txs(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL,
  wallet_id INTEGER NOT NULL,
  from_address TEXT NOT NULL,
  to_address TEXT NOT NULL,
  amount_wei TEXT NOT NULL DEFAULT '0',
  status TEXT NOT NULL DEFAULT 'pending',
  tx_hash TEXT,
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_activity_created ON activity(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_nft_wallet ON nft_cache(wallet_id, chain_id);
CREATE INDEX IF NOT EXISTS idx_elig_checked ON eligibility_checks(checked_at DESC);
CREATE INDEX IF NOT EXISTS idx_fund_txs_job ON fund_txs(job_id, seq);
CREATE INDEX IF NOT EXISTS idx_fund_jobs_status ON fund_jobs(status);
"#;

/// Curated default set. Extra chains can be added later via Settings.
pub const DEFAULT_CHAINS: &[(&str, i64, &str, &str, &str)] = &[
    ("Ethereum", 1, "https://cloudflare-eth.com", "ETH", "https://etherscan.io"),
    ("BNB Chain", 56, "https://bsc.meowrpc.com", "BNB", "https://bscscan.com"),
    ("Base", 8453, "https://mainnet.base.org", "ETH", "https://basescan.org"),
    ("Ink", 57073, "https://rpc-gel.inkonchain.com", "ETH", "https://explorer.inkonchain.com"),
    (
        "Robinhood Chain",
        4663,
        "https://rpc.mainnet.chain.robinhood.com",
        "ETH",
        "https://robinhoodchain.blockscout.com",
    ),
    (
        "Sepolia",
        11155111,
        "https://sepolia.gateway.tenderly.co",
        "ETH",
        "https://sepolia.etherscan.io",
    ),
];

/// Former stock defaults that are no longer part of `DEFAULT_CHAINS`.
/// `seed_chains` disables them once (meta flag) so existing installs match
/// the curated set; the user can re-enable / re-add them later.
pub const REMOVED_DEFAULT_CHAIN_IDS: &[i64] = &[
    10,      // Optimism
    137,     // Polygon
    42161,   // Arbitrum One
    5042002, // Arc Testnet
];

/// Old default RPC URLs that are known dead/rate-limited. `seed_chains`
/// refreshes these to the current default when a row still points at them.
/// User-customized URLs are never overwritten. Only chains still present
/// in `DEFAULT_CHAINS` may appear here.
pub const RETIRED_DEFAULT_RPCS: &[(i64, &[&str])] = &[
    (1, &["https://eth.llamarpc.com"]),
    (
        56,
        &[
            "https://bsc-dataseed.binance.org",
            "https://bsc-rpc.publicnode.com",
        ],
    ),
    (
        11155111,
        &[
            "https://rpc.sepolia.org",
            "https://ethereum-sepolia-rpc.publicnode.com",
            "https://ethereum-sepolia.publicnode.com",
        ],
    ),
];
