pub const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS meta(
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wallets(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  label TEXT NOT NULL,
  address TEXT NOT NULL UNIQUE,
  enc_privkey BLOB NOT NULL,
  nonce BLOB NOT NULL,
  created_at INTEGER NOT NULL
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
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS eligibility_checks(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  wallet_id INTEGER,
  collection TEXT NOT NULL,
  result INTEGER NOT NULL,
  detail TEXT,
  checked_at INTEGER NOT NULL
);

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

CREATE INDEX IF NOT EXISTS idx_activity_created ON activity(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_nft_wallet ON nft_cache(wallet_id, chain_id);
CREATE INDEX IF NOT EXISTS idx_elig_checked ON eligibility_checks(checked_at DESC);
"#;

pub const DEFAULT_CHAINS: &[(&str, i64, &str, &str, &str)] = &[
    ("Ethereum", 1, "https://eth.llamarpc.com", "ETH", "https://etherscan.io"),
    ("BNB Chain", 56, "https://bsc-dataseed.binance.org", "BNB", "https://bscscan.com"),
    ("Base", 8453, "https://mainnet.base.org", "ETH", "https://basescan.org"),
    ("Arbitrum One", 42614, "https://arb1.arbitrum.io/rpc", "ETH", "https://arbiscan.io"),
    ("Optimism", 10, "https://mainnet.optimism.io", "OP", "https://optimistic.etherscan.io"),
    ("Polygon", 137, "https://polygon-rpc.com", "POL", "https://polygonscan.com"),
    ("Sepolia", 11155111, "https://rpc.sepolia.org", "ETH", "https://sepolia.etherscan.io"),
];
