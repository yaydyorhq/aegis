export interface WalletRow {
  id: number;
  label: string;
  address: string;
  created_at: number;
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
  status: string;
  tx_hash: string | null;
  error: string | null;
  created_at: number;
  updated_at: number;
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
}

export interface StatsOverview {
  wallets: number;
  tasks: number;
  rpc_endpoints: number;
  eligible_checks: number;
  activity_records: number;
  vault_unlocked: boolean;
  api_providers: number;
}

export interface ModuleStatusItem {
  label: string;
  status: string;
  detail: string;
  ok: boolean;
}
