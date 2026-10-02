import { useId } from "react";

/**
 * Aegis shield mark — gradient shield with the carved "A" chevron.
 * Scales crisply at any size (sidebar brand, lock screen, future uses);
 * the app icon carries the same mark on a dark rounded tile.
 */
export function LogoMark({ className }: { className?: string }) {
  const id = useId();
  return (
    <svg viewBox="0 0 64 64" className={className} aria-hidden="true">
      <defs>
        <linearGradient id={id} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#6b95ff" />
          <stop offset="1" stopColor="#3050d8" />
        </linearGradient>
      </defs>
      <path
        d="M32 4 L54 12.5 V30.5 C54 45 44.5 55.5 32 60 C19.5 55.5 10 45 10 30.5 V12.5 Z"
        fill={`url(#${id})`}
      />
      <path
        d="M22.5 44 L32 18.5 L41.5 44"
        fill="none"
        stroke="#0b0b10"
        strokeWidth="6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
