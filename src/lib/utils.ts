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
