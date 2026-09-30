import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function shortAddress(addr: string, size = 4): string {
  if (!addr || addr.length < size * 2 + 2) return addr;
  return `${addr.slice(0, 2 + size)}…${addr.slice(-size)}`;
}

export function greeting(d = new Date()): string {
  const h = d.getHours();
  if (h < 5) return "good night";
  if (h < 12) return "good morning";
  if (h < 18) return "good afternoon";
  return "good evening";
}

/**
 * Trim a backend ETH decimal string (exact wei math → up to 18 decimals) to a
 * readable display value: ≥0.0001 → max 4 decimals, smaller → 2 significant
 * digits. A leading sign is preserved ("+1.5" stays positive — flow coloring
 * keys off it); trailing zeros are stripped. Non-numeric input passes through.
 */
export function formatEth(v: string | null | undefined): string {
  if (!v) return "0";
  const s = v.trim();
  const plus = s.startsWith("+");
  const neg = s.startsWith("-");
  const body = s.replace(/^[+-]/, "");
  if (body === "" || !/^\d*\.?\d*$/.test(body) || Number.isNaN(Number(body))) {
    return s;
  }
  const n = Number(body);
  if (n === 0) return "0";
  let out = n >= 0.0001 ? n.toFixed(4) : n.toPrecision(2);
  if (!out.includes("e")) {
    out = out.replace(/(\.\d*?)0+$/, "$1").replace(/\.$/, "");
  }
  const sign = neg && Number(out) !== 0 ? "-" : plus ? "+" : "";
  return sign + out;
}
