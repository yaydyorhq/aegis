# Aegis

Dark Web3 / EVM command center — Tauri 2 + React + TypeScript + Tailwind + SQLite.

Local vault (Argon2id + AES-256-GCM), multi-wallet, mint queue, NFT scanner, eligibility checks, multi-chain RPC.

## Develop

```bash
npm install
npm run tauri dev
```

## Build

```bash
npm run tauri build
```

Installers land in `src-tauri/target/release/bundle/`.

## Stack

- Frontend: React 19, Vite, Tailwind v4, zustand
- Backend: Tauri 2 (Rust), rusqlite, alloy, argon2, aes-gcm
- Secrets never leave the device
