# NEAR Flows frontend

Draft workspace for phases 1 and 2 plus recorded replay of a `DEMO|` log.

```bash
cd frontend
npm install
npm test
npm run dev
```

The dev server does not connect a wallet, read `.env`, or start `scripts/demo/run.sh`.
Import a log on the Demo page only if you already have one. Nothing in the UI is submitted.

`npm run build` emits a static site with base `/near-flows/` and copies `index.html` to `404.html` so GitHub Pages can open client routes. Local `npm run dev` stays at `/`.

Source commit this UI was checked against: `9608b9df7a7ec6f13eb10e28fc0aabd297ff12a7`.
