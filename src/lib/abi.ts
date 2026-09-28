/** Contract ABI helpers for the Create Task form.
 *
 *  Sourcify is used as the source of truth (free, CORS-open, keyed by chain
 *  id + address).  Proxy contracts (EIP-1967 / beacon / EIP-897) only expose
 *  their admin interface, so we follow the implementation slot first when the
 *  fetched ABI looks like a proxy.
 */

export interface AbiInput {
  name: string;
  type: string;
}

export interface AbiFn {
  name: string;
  inputs: AbiInput[];
  stateMutability: string;
}

export interface AbiInfo {
  /** Address the ABI actually describes (implementation address when proxied). */
  address: string;
  viaProxy: boolean;
  /** e.g. "Sourcify exact-match" */
  source: string;
  fns: AbiFn[];
}

const EIP1967_IMPL_SLOT =
  "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc";
const EIP1967_BEACON_SLOT =
  "0xa3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50";
/** Pre-EIP-1967 proxy slot still used by older OpenZeppelin/Zeppelinos proxies. */
const LEGACY_IMPL_SLOT =
  "0x7050c9e0f4ca769c69bd3a8ef740bc37934f8e2c036e5a723fd8ee048ed3f8c3";
const IMPLEMENTATION_SELECTOR = "0x5c60da1b"; // implementation()

/** Read-only public endpoints used when the chain's own RPC is dead.
 *  (cloudflare-eth.com has been returning -32603 for eth_getStorageAt.) */
const RPC_FALLBACKS: Record<number, string[]> = {
  1: [
    "https://ethereum-rpc.publicnode.com",
    "https://1rpc.io/eth",
    "https://eth.drpc.org",
  ],
};

const cache = new Map<string, Promise<AbiInfo | null>>();

export function isAddress(v: string): boolean {
  return /^0x[0-9a-fA-F]{40}$/.test(v.trim());
}

/** Canonical signature used both as the <option> value and by the encoder. */
export function fnSignature(fn: AbiFn): string {
  return `${fn.name}(${fn.inputs.map((i) => i.type).join(",")})`;
}

/** "to (address); quantity (uint256)" — shown next to the Parameters field. */
export function fnHint(fn: AbiFn | null): string {
  if (!fn || fn.inputs.length === 0) return "no parameters";
  return fn.inputs
    .map((input, i) => `${input.name || `arg${i}`} (${input.type})`)
    .join("; ");
}

/** Prefill for the Parameters field — placeholders resolve at encode time. */
export function fnDefaults(fn: AbiFn): string {
  return fn.inputs.map((input) => defaultFor(input.type)).join("; ");
}

function defaultFor(type: string): string {
  if (type.startsWith("address")) return "{address}";
  if (/^u?int/.test(type)) return "{quantity}";
  if (type === "bool") return "true";
  if (type === "string") return "";
  if (/^bytes\d+$/.test(type)) {
    const n = Number(type.slice(5));
    return Number.isFinite(n) && n > 0 ? "0x" + "00".repeat(n) : "0x";
  }
  if (type.startsWith("bytes")) return "0x";
  if (type.endsWith("[]")) return ""; // empty array
  return "";
}

export function callableFns(abi: unknown): AbiFn[] {
  if (!Array.isArray(abi)) return [];
  const out: AbiFn[] = [];
  const seen = new Set<string>();
  for (const raw of abi) {
    if (!raw || raw.type !== "function") continue;
    const mut = String(raw.stateMutability ?? "");
    if (mut === "view" || mut === "pure") continue;
    const fn: AbiFn = {
      name: String(raw.name ?? ""),
      inputs: Array.isArray(raw.inputs)
        ? raw.inputs.map((i: { name?: string; type?: string }) => ({
            name: String(i?.name ?? ""),
            type: String(i?.type ?? "address"),
          }))
        : [],
      stateMutability: mut || (raw.payable ? "payable" : "nonpayable"),
    };
    if (!fn.name) continue;
    const sig = fnSignature(fn);
    if (seen.has(sig)) continue;
    seen.add(sig);
    out.push(fn);
  }
  return out;
}

/** True when the ABI only exposes admin/proxy surface (BeaconProxy, ERC1967…). */
function looksLikeProxy(abi: unknown): boolean {
  if (!Array.isArray(abi)) return false;
  const fns = new Set<string>();
  const events = new Set<string>();
  for (const raw of abi) {
    if (!raw) continue;
    if (raw.type === "function" && raw.name) fns.add(String(raw.name));
    if (raw.type === "event" && raw.name) events.add(String(raw.name));
  }
  for (const name of ["upgradeTo", "upgradeToAndCall", "admin", "changeAdmin"]) {
    if (fns.has(name)) return true;
  }
  for (const name of ["AdminChanged", "BeaconUpgraded", "Upgraded"]) {
    if (events.has(name)) return true;
  }
  return false;
}

async function rpcCall(rpcUrl: string, method: string, params: unknown[]): Promise<string> {
  const res = await fetch(rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  if (!res.ok) throw new Error(`RPC HTTP ${res.status}`);
  const body = (await res.json()) as { result?: string; error?: { message?: string } };
  if (body.error) throw new Error(body.error.message ?? "RPC error");
  return String(body.result ?? "");
}

function wordToAddress(word: string): string | null {
  const w = word.trim().toLowerCase();
  if (!/^0x[0-9a-f]{64}$/.test(w)) return null;
  const addr = "0x" + w.slice(-40);
  if (addr === "0x" + "0".repeat(40)) return null;
  return addr;
}

async function firstOk(candidates: string[], call: (rpc: string) => Promise<string>): Promise<string> {
  let lastErr: unknown = null;
  for (const rpc of candidates) {
    try {
      return await call(rpc);
    } catch (e) {
      lastErr = e;
    }
  }
  throw lastErr ?? new Error("no RPC available");
}

/** EIP-1167 minimal-clone stubs: prefix + impl address + delegatecall tail. */
const EIP1167_TAIL = "5af43d82803e903d91602b57fd5bf3";
const EIP1167_PREFIXES = ["363d3d373d3d3d363d73", "3d3d3d3d363d73"];

/** Runtime code → implementation address when it is a 45-byte clone stub. */
export function stubTarget(code: string): string | null {
  const body = code.trim().toLowerCase().replace(/^0x/, "");
  for (const prefix of EIP1167_PREFIXES) {
    if (!body.startsWith(prefix)) continue;
    if (body.length !== prefix.length + 40 + EIP1167_TAIL.length) continue;
    if (!body.endsWith(EIP1167_TAIL)) continue;
    const impl = body.slice(prefix.length, prefix.length + 40);
    if (/^[0-9a-f]{40}$/.test(impl)) return "0x" + impl;
  }
  return null;
}

async function cloneTarget(candidates: string[], address: string): Promise<string | null> {
  const code = await firstOk(candidates, (rpc) =>
    rpcCall(rpc, "eth_getCode", [address, "latest"]),
  ).catch(() => null);
  return code ? stubTarget(code) : null;
}

function rpcCandidates(chainId: number, rpcUrl: string | null): string[] {
  return [...new Set<string>(
    [rpcUrl, ...(RPC_FALLBACKS[chainId] ?? [])].filter((u): u is string => Boolean(u)),
  )];
}

async function resolveImplementation(candidates: string[], address: string): Promise<string | null> {
  // Minimal clone first: its runtime code names the implementation directly,
  // so no storage slot or `implementation()` call is needed.
  const clone = await cloneTarget(candidates, address).catch(() => null);
  if (clone) return clone;

  for (const slot of [EIP1967_IMPL_SLOT, LEGACY_IMPL_SLOT]) {
    const word = await firstOk(candidates, (rpc) =>
      rpcCall(rpc, "eth_getStorageAt", [address, slot, "latest"]),
    ).catch(() => null);
    const impl = word ? wordToAddress(word) : null;
    if (impl) return impl;
  }

  const beaconWord = await firstOk(candidates, (rpc) =>
    rpcCall(rpc, "eth_getStorageAt", [address, EIP1967_BEACON_SLOT, "latest"]),
  ).catch(() => null);
  const beacon = beaconWord ? wordToAddress(beaconWord) : null;
  if (beacon) {
    try {
      const out = await firstOk(candidates, (rpc) =>
        rpcCall(rpc, "eth_call", [{ to: beacon, data: IMPLEMENTATION_SELECTOR }, "latest"]),
      );
      const impl = wordToAddress(out);
      if (impl) return impl;
    } catch {
      /* fall through to EIP-897 */
    }
  }

  try {
    const out = await firstOk(candidates, (rpc) =>
      rpcCall(rpc, "eth_call", [{ to: address, data: IMPLEMENTATION_SELECTOR }, "latest"]),
    );
    return wordToAddress(out);
  } catch {
    return null;
  }
}

function sourceLabel(body: {
  match?: unknown;
  creationMatch?: unknown;
  runtimeMatch?: unknown;
}): string {
  const parts = [body.match, body.runtimeMatch, body.creationMatch]
    .map((v) => String(v ?? "").toLowerCase())
    .filter(Boolean);
  if (parts.includes("exact") || parts.includes("exact-match")) return "Sourcify exact-match";
  if (parts.includes("partial") || parts.includes("partial-match")) return "Sourcify partial-match";
  if (parts.includes("match")) return "Sourcify exact-match";
  if (parts.length > 0) return `Sourcify ${parts[0]}`;
  return "Sourcify verified";
}

async function fetchSourcify(
  chainId: number,
  address: string,
): Promise<{ abi: unknown; source: string } | null> {
  const url =
    `https://sourcify.dev/server/v2/contract/${chainId}/${address}` +
    `?fields=abi,creationMatch,runtimeMatch`;
  const res = await fetch(url, { headers: { accept: "application/json" } });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error(`Sourcify HTTP ${res.status}`);
  const body = (await res.json()) as Record<string, unknown>;
  if (!Array.isArray(body.abi)) return null;
  return { abi: body.abi, source: sourceLabel(body) };
}

/** Admin surface of a proxy — never a mint candidate. */
const PROXY_ADMIN_FNS = new Set([
  "admin",
  "changeAdmin",
  "implementation",
  "upgrade",
  "upgradeTo",
  "upgradeToAndCall",
  "beacon",
  "token",
]);

function shortAddr(addr: string): string {
  return `${addr.slice(0, 6)}…${addr.slice(-4)}`;
}

async function resolve(chainId: number, address: string, rpcUrl: string | null): Promise<AbiInfo | null> {
  let direct: { abi: unknown; source: string } | null = null;
  try {
    direct = await fetchSourcify(chainId, address);
  } catch {
    return null;
  }
  if (!direct) {
    // Target itself is unverified — a clone/proxy usually keeps its source at
    // the implementation, so follow it before giving up ("presets only").
    const candidates = rpcCandidates(chainId, rpcUrl);
    if (candidates.length > 0) {
      const impl = await resolveImplementation(candidates, address).catch(() => null);
      if (impl && impl.toLowerCase() !== address.toLowerCase()) {
        const via = await fetchSourcify(chainId, impl).catch(() => null);
        const fns = via ? callableFns(via.abi) : [];
        if (via && fns.length > 0) {
          return { address: impl, viaProxy: true, source: via.source, fns };
        }
      }
    }
    return null;
  }

  const proxied = looksLikeProxy(direct.abi);
  // A proxy's own ABI only describes admin surface — strip it so users can't
  // pick upgradeTo() as their "mint" function.
  const directFns = proxied
    ? callableFns(direct.abi).filter((f) => !PROXY_ADMIN_FNS.has(f.name))
    : callableFns(direct.abi);

  if (proxied || directFns.length === 0) {
    const candidates = rpcCandidates(chainId, rpcUrl);
    let impl: string | null = null;
    if (candidates.length > 0) {
      impl = await resolveImplementation(candidates, address).catch(() => null);
    }
    if (impl && impl.toLowerCase() !== address.toLowerCase()) {
      const via = await fetchSourcify(chainId, impl).catch(() => null);
      if (via) {
        const fns = callableFns(via.abi);
        if (fns.length > 0) {
          return { address: impl, viaProxy: true, source: via.source, fns };
        }
      }
      return {
        address: impl,
        viaProxy: true,
        source: `proxy · impl ${shortAddr(impl)} unverified`,
        fns: directFns,
      };
    }
    if (directFns.length === 0) {
      return {
        address,
        viaProxy: Boolean(impl) || proxied,
        source: "proxy · implementation ABI unavailable",
        fns: [],
      };
    }
  }

  if (directFns.length === 0) return null;
  return { address, viaProxy: false, source: direct.source, fns: directFns };
}

/** Cached ABI lookup — repeated keystrokes in the contract field don't refetch. */
export function loadAbi(
  chainId: number,
  address: string,
  rpcUrl: string | null,
): Promise<AbiInfo | null> {
  const key = `${chainId}:${address.trim().toLowerCase()}`;
  let hit = cache.get(key);
  if (!hit) {
    hit = resolve(chainId, address.trim(), rpcUrl).catch(() => null);
    cache.set(key, hit);
  }
  return hit;
}
