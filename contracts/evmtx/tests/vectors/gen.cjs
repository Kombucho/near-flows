const v = require('viem');
const router = '0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48E';
const weth = '0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14';
const usdc = '0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238';
const me = '0x00000000000000000000000000000000000000ab';
const approve = v.encodeFunctionData({ abi: v.parseAbi(['function approve(address,uint256) returns (bool)']), functionName: 'approve', args: [router, 1000000n] });
const swap = v.encodeFunctionData({ abi: v.parseAbi(['struct P { address tokenIn; address tokenOut; uint24 fee; address recipient; uint256 amountIn; uint256 amountOutMinimum; uint160 sqrtPriceLimitX96; }', 'function exactInputSingle(P params) payable returns (uint256)']),
  functionName: 'exactInputSingle', args: [{ tokenIn: usdc, tokenOut: weth, fee: 3000, recipient: me, amountIn: 1000000n, amountOutMinimum: 123456789n, sqrtPriceLimitX96: 0n }] });
const tx = { type: 'eip1559', chainId: 11155111, nonce: 7, maxPriorityFeePerGas: 1500000000n, maxFeePerGas: 30000000000n, gas: 250000n, to: router, value: 0n, data: swap };
const unsigned = v.serializeTransaction(tx);
const signed = v.serializeTransaction(tx, { r: '0x' + '11'.repeat(32), s: '0x' + '22'.repeat(32), yParity: 1 });
const big = { ...tx, nonce: 0, value: 0n, data: '0x' };
console.log(JSON.stringify({ approve, swap, unsigned, hash: v.keccak256(unsigned), signed, zero_nonce_empty_data: v.serializeTransaction(big),
  addr_of_pub: require('viem/accounts').publicKeyToAddress('0x04' + 'ab'.repeat(64)) }, null, 1));
