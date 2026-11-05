// Run the chat server.
//   BAZAAR=bazaar.testnet RPC=https://rpc.testnet.near.org RECIPIENT=chat.example.com PORT=8787 npm start

import { Auth } from "./auth.ts";
import { Chain } from "./chain.ts";
import { Rooms } from "./rooms.ts";
import { startServer } from "./server.ts";

const env = (k: string, d?: string) => process.env[k] ?? d ?? (() => { throw new Error(`set ${k}`); })();
const chain = new Chain(env("RPC", "https://rpc.testnet.near.org"), env("BAZAAR"));
// RECIPIENT must be what the app asks wallets to sign for (usually the app's domain).
const auth = new Auth(env("RECIPIENT"), chain);
const rooms = new Rooms(env("DB", "chat.sqlite"), chain);
const port = Number(env("PORT", "8787"));
await startServer(auth, rooms, port, env("ALLOW_ORIGIN", "*"));
console.log(`chat on :${port}, bazaar ${env("BAZAAR")}`);
