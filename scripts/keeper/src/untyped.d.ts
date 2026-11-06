// bn.js and bs58 come in through the Solana SDKs without type declarations;
// fork-sol.ts only passes their values on to those SDKs, so loose types are enough.
declare module "bn.js" {
  class BN {
    constructor(n: number | string | bigint, base?: number);
    muln(n: number): BN;
    divn(n: number): BN;
    toString(base?: number): string;
    [k: string]: any;
  }
  export default BN;
}
declare module "bs58" {
  const bs58: { encode(b: Uint8Array): string; decode(s: string): Uint8Array };
  export default bs58;
}
