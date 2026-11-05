// HTTP + WebSocket front of the chat.
//
//   GET  /challenge            -> { nonce, message, recipient }  (sign it with your wallet, NEP-413)
//   POST /login                -> { token }                      body: { accountId, publicKey, signature, nonce }
//   WS   /ws?token=...         then JSON frames:
//        { "type": "join",    "room": "lot:7" }          -> history, then live messages
//        { "type": "say",     "room": "lot:7", "text": "gm" }
//        { "type": "history", "room": "lot:7", "before": 120 }
//        { "type": "leave",   "room": "lot:7" }
// Server frames: { type: "msg", msg } | { type: "history", room, msgs } | { type: "joined", room } | { type: "error", error }
// Text is plain text: clients must render it as text, never as HTML.

import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { WebSocketServer, type WebSocket } from "ws";
import type { Auth } from "./auth.ts";
import type { Rooms } from "./rooms.ts";

const MAX_BODY = 4_096;
const MAX_ROOMS_PER_SOCKET = 20;
const MAX_SOCKETS_PER_ACCOUNT = 5;

export function startServer(auth: Auth, rooms: Rooms, port: number, allowOrigin = "*"): Promise<Server> {
  const subs = new Map<string, Set<WebSocket>>(); // room -> sockets
  const perAccount = new Map<string, number>();

  const http = createServer(async (req, res) => {
    res.setHeader("Access-Control-Allow-Origin", allowOrigin);
    res.setHeader("Access-Control-Allow-Headers", "content-type");
    if (req.method === "OPTIONS") return end(res, 204, {});
    try {
      if (req.method === "GET" && req.url === "/challenge") return end(res, 200, auth.challenge());
      if (req.method === "POST" && req.url === "/login") {
        const body = JSON.parse(await readBody(req));
        return end(res, 200, { token: await auth.login(body) });
      }
      end(res, 404, { error: "not found" });
    } catch (e) {
      end(res, 400, { error: (e as Error).message });
    }
  });

  const wss = new WebSocketServer({ noServer: true, maxPayload: MAX_BODY });
  http.on("upgrade", (req, socket, head) => {
    const url = new URL(req.url ?? "/", "http://x");
    const who = url.pathname === "/ws" ? auth.whoIs(url.searchParams.get("token")) : null;
    if (!who || (perAccount.get(who) ?? 0) >= MAX_SOCKETS_PER_ACCOUNT) {
      socket.write("HTTP/1.1 401 Unauthorized\r\n\r\n");
      return socket.destroy();
    }
    wss.handleUpgrade(req, socket, head, (ws) => onSocket(ws, who));
  });

  function onSocket(ws: WebSocket, who: string) {
    perAccount.set(who, (perAccount.get(who) ?? 0) + 1);
    const joined = new Set<string>();
    const send = (o: unknown) => ws.readyState === ws.OPEN && ws.send(JSON.stringify(o));
    ws.on("message", async (raw) => {
      try {
        const f = JSON.parse(raw.toString());
        const room = f.room;
        switch (f.type) {
          case "join": {
            if (joined.size >= MAX_ROOMS_PER_SOCKET) throw new Error("too many rooms");
            await rooms.assertCanEnter(room, who);
            joined.add(room);
            if (!subs.has(room)) subs.set(room, new Set());
            subs.get(room)!.add(ws);
            send({ type: "joined", room });
            send({ type: "history", room, msgs: await rooms.history(room, who) });
            break;
          }
          case "say": {
            if (!joined.has(room)) throw new Error("join the room first");
            const msg = await rooms.say(room, who, f.text); // sender = the signed-in account
            for (const s of subs.get(room) ?? []) if (s.readyState === s.OPEN) s.send(JSON.stringify({ type: "msg", msg }));
            break;
          }
          case "history":
            send({ type: "history", room, msgs: await rooms.history(room, who, f.before) });
            break;
          case "leave":
            joined.delete(room);
            subs.get(room)?.delete(ws);
            break;
          default:
            throw new Error("unknown frame");
        }
      } catch (e) {
        send({ type: "error", error: (e as Error).message });
      }
    });
    ws.on("close", () => {
      for (const r of joined) subs.get(r)?.delete(ws);
      perAccount.set(who, (perAccount.get(who) ?? 1) - 1);
    });
  }

  return new Promise((resolve) => http.listen(port, () => resolve(http)));
}

function end(res: ServerResponse, status: number, body: unknown) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

function readBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    let data = "";
    req.on("data", (c) => {
      data += c;
      if (data.length > MAX_BODY) {
        reject(new Error("body too large"));
        req.destroy();
      }
    });
    req.on("end", () => resolve(data));
    req.on("error", reject);
  });
}
