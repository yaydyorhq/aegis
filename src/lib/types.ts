export interface WalletRow {
  id: number;
  label: string;
  address: string;
  created_at: number;
  group_id: number | null;
}

export interface WalletGroup {
  id: number;
  name: string;
  created_at: number;
  wallet_count: number;
}

export interface BulkImportItem {
  label?: string | null;
  private_key: string;
}

export interface BulkImportResultItem {
  index: number;
  status: "imported" | "duplicate" | "invalid";
  address: string | null;
  label: string | null;
  error: string | null;
}

export interface ChainRow {
  id: number;
  name: string;
  chain_id: number;
  rpc_url: string;
  symbol: string;
  explorer: string | null;
  enabled: number;
}

export interface ActivityRow {
  id: number;
  kind: string;
  summary: string;
  payload: string | null;
  ok: number;
  created_at: number;
}

export interface MintTaskRow {
  id: number;
  chain_id: number;
  contract: string;
  quantity: number;
  value_wei: string | null;
  calldata?: string | null;
  status: string;
  tx_hash: string | null;
  error: string | null;
  wallet_id?: number | null;
  created_at: number;
  updated_at: number;
  function_name?: string | null;
  is_hex?: boolean;
  parameters?: string | null;
  rpc_endpoints?: string | null;
  flashbots?: boolean;
  gas_limit?: string | null;
  max_fee_gwei?: string | null;
  priority_fee_gwei?: string | null;
  nonce_override?: string | null;
  scheduled_at?: number | null;
  delay_ms?: number;
  mode?: string;
  poll_attempts?: number;
}

export interface EligibilityRow {
  id: number;
  wallet_id: number | null;
  wallet_address?: string;
  collection: string;
  result: number;
  detail: string | null;
  checked_at: number;
}

export interface NftItem {
  id: number;
  wallet_id: number | null;
  chain_id: number;
  contract: string;
  token_id: string;
  uri: string | null;
  image?: string | null;
  name?: string | null;
  opensea_url?: string | null;
  fetched_at: number;
}

export interface VaultStatus {
  initialized: boolean;
  unlocked: boolean;
}

export interface RpcTestResult {
  ok: boolean;
  chain_id_returned: number | null;
  latency_ms: number | null;
  error: string | null;
  /** Whether this endpoint supports eth_call (contract interaction). */
  eth_call_ok?: boolean | null;
}

export interface PublicDrop {
  mint_price_wei: string;
  start_time: number;
  end_time: number;
  max_per_wallet: number;
  fee_bps: number;
  restrict_fee_recipients: boolean;
}

export interface SeaDropPlan {
  to: string;
  nft_contract: string;
  fee_recipient: string;
  fee_source: string;
  calldata: string;
  value_wei: string;
  value_eth: string;
  quantity: number;
  drop: PublicDrop;
  live: boolean;
}

export interface OpenSeaMintPlan {
  to: string;
  calldata: string;
  value_wei: string;
  value_eth: string;
  quantity: number;
  stage_type: string;
  stage_index: number;
  slug: string;
  network_id: number;
  nft_contract: string;
}

export interface AllowlistEntryInfo {
  address: string;
  has_proof: boolean;
  has_signature: boolean;
  has_calldata: boolean;
}

export interface AllowlistMatchRow {
  wallet_id: number;
  address: string;
  matched: boolean;
  has_proof: boolean;
  has_signature: boolean;
  has_calldata: boolean;
}

export interface SkippedWallet {
  wallet_id: number;
  address: string;
  reason: string;
}

export interface EnqueueBatchResult {
  tasks: MintTaskRow[];
  skipped: SkippedWallet[];
}

export interface MintOpenSeaEnqueueArgs {
  wallet_ids: number[];
  chain_id: number;
  collection: string;
  quantity: number;
  token_id?: string | null;
  rpc_endpoints?: string[] | null;
  flashbots?: boolean;
  gas_limit?: string | null;
  max_fee_gwei?: string | null;
  priority_fee_gwei?: string | null;
  scheduled_at?: number | null;
  delay_ms?: number | null;
  mode?: string | null;
}

export interface PortfolioCollectionSummary {
  contract: string;
  chain_id: number;
  native_symbol: string;
  net_eth: string | null;
  roi_pct: number | null;
  spent_eth: string;
  gas_eth: string;
  holding: number;
  balance: number;
  floor_eth: string | null;
  floor_source: string | null;
  scanned_at: number;
  wallets: number;
}

export interface PortfolioStats {
  wallets: number;
  pairs: number;
  net_flow_eth: string | null;
  latest_at: number | null;
  collection: PortfolioCollectionSummary | null;
}

export interface StatsOverview {
  wallets: number;
  tasks: number;
  rpc_endpoints: number;
  eligible_checks: number;
  activity_records: number;
  vault_unlocked: boolean;
  api_providers: number;
  portfolio: PortfolioStats;
}

export interface ModuleStatusItem {
  label: string;
  status: string;
  detail: string;
  ok: boolean;
}

// ── Manage Funds (disperse / consolidate) ──────────────────────────

export interface FundsStartArgs {
  mode: "disperse" | "consolidate";
  amount_mode: "fixed" | "target";
  asset: string;
  chain_id: number;
  amount_wei: string;
  anchor_wallet_id: number;
  peer_wallet_ids: number[];
}

export interface FundJobRow {
  id: number;
  mode: string;
  amount_mode: string;
  asset: string;
  chain_id: number;
  amount_wei: string;
  anchor_wallet_id: number;
  peer_wallet_ids: number[];
  status: string;
  total_count: number;
  success_count: number;
  failed_count: number;
  created_at: number;
  updated_at: number;
}

export interface FundTxRow {
  id: number;
  job_id: number;
  seq: number;
  kind: string;
  wallet_id: number;
  from_address: string;
  to_address: string;
  amount_wei: string;
  status: string;
  tx_hash: string | null;
  error: string | null;
  created_at: number;
  updated_at: number;
}

export interface FundPreviewRow {
  wallet_id: number;
  from_address: string;
  to_address: string;
  balance_wei: string;
  send_wei: string;
  skip_reason: string | null;
}

export interface FundPreview {
  rows: FundPreviewRow[];
  total_send_wei: string;
  is_native: boolean;
  decimals: number;
  symbol: string;
  warnings: string[];
}

// ── PnL ─────────────────────────────────────────────────────────────

export interface PnlToken {
  contract: string;
  symbol: string;
  decimals: number;
  balance: string;
  net_flow: string;
}

export interface PnlResult {
  wallet_id: number;
  chain_id: number;
  status: string;
  source: string;
  native_symbol: string;
  native_balance: string;
  net_native_flow: string | null;
  tokens: PnlToken[];
  unrealized_eth: string;
  realized_eth: string;
  note: string | null;
  scanned_at: number;
  window_blocks: number;
  from_block: number;
  to_block: number;
}

export interface PnlHistoryRow {
  id: number;
  wallet_id: number;
  chain_id: number;
  status: string;
  scanned_at: number;
  summary: {
    source?: string;
    native_symbol?: string;
    native_balance?: string;
    net_native_flow?: string | null;
    token_count?: number;
    window_blocks?: number;
    from_block?: number;
    to_block?: number;
  } | null;
}

// ── Collection PnL ───────────────────────────────────────────────

export interface CollectionPnlRow {
  wallet_id: number | null;
  label: string;
  address: string;
  minted: number;
  bought: number;
  sold: number;
  holding: number;
  balance: number;
  balance_known: boolean;
  out_of_window: number;
  spent_eth: string;
  gas_eth: string;
  realized_eth: string;
  unrealized_eth: string | null;
  net_eth: string | null;
  roi_pct: number | null;
  basis_incomplete: boolean;
  unpriced_sales: number;
  transfer_in: number;
  transfer_out: number;
}

export interface CollectionPnlTotals {
  wallets: number;
  minted: number;
  bought: number;
  sold: number;
  holding: number;
  balance: number;
  spent_eth: string;
  gas_eth: string;
  realized_eth: string;
  unrealized_eth: string | null;
  net_eth: string | null;
  roi_pct: number | null;
  basis_incomplete: boolean;
  unpriced_sales: number;
}

export interface CollectionPnlResult {
  contract: string;
  chain_id: number;
  native_symbol: string;
  window_blocks: number;
  from_block: number;
  to_block: number;
  floor_eth: string | null;
  floor_source: string | null;
  fee_bps: number;
  rows: CollectionPnlRow[];
  totals: CollectionPnlTotals;
  warnings: string[];
  scanned_at: number;
}

/** Per-stage eligibility cell for one wallet. */
export interface StageMatrixCell {
  stage_name: string;
  eligible: boolean;
  max_quantity: number | null;
  price_usd: number | null;
  /** Stage open time (unix ms) when OpenSea reports it. */
  starts_at_ms: number | null;
  /** "live" | "not_started" | "unknown" */
  live_status: string;
  /** Human hint: "Live now" / "Opens Sep 28 12:00 UTC" / "—" */
  schedule_hint: string;
  /** eligible AND live — only actionable cells can be queued. */
  actionable: boolean;
}

/** One wallet row in the eligibility matrix. */
export interface StageMatrixRow {
  wallet_id: number | null;
  address: string;
  collection: string;
  slug: string;
  stages: StageMatrixCell[];
  error: string | null;
  checked_at: number;
}

/** Full matrix result from eligibility_matrix_run. */
export interface StageMatrixResult {
  slug: string;
  collection: string;
  stage_names: string[];
  rows: StageMatrixRow[];
  checked_at: number;
}
