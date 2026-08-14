const IV = new Uint32Array([
  0x6a09e667,
  0xbb67ae85,
  0x3c6ef372,
  0xa54ff53a,
  0x510e527f,
  0x9b05688c,
  0x1f83d9ab,
  0x5be0cd19,
]);

const MESSAGE_PERMUTATION = [
  2,
  6,
  3,
  10,
  7,
  0,
  4,
  13,
  1,
  11,
  12,
  5,
  9,
  14,
  15,
  8,
] as const;

const CHUNK_START = 1;
const CHUNK_END = 2;
const PARENT = 4;
const ROOT = 8;
const BLOCK_LENGTH = 64;
const CHUNK_LENGTH = 1024;

/** Computes the 32-byte BLAKE3 digest used by application artifact identity. */
export function blake3Hex(bytes: Uint8Array): string {
  const outputs: Output[] = [];
  const chunkCount = Math.max(1, Math.ceil(bytes.length / CHUNK_LENGTH));
  for (let chunk = 0; chunk < chunkCount; chunk += 1) {
    const start = chunk * CHUNK_LENGTH;
    const end = Math.min(bytes.length, start + CHUNK_LENGTH);
    outputs.push(chunkOutput(bytes.subarray(start, end), chunk));
  }

  let level = outputs;
  while (level.length > 1) {
    const next: Output[] = [];
    for (let index = 0; index < level.length; index += 2) {
      const left = level[index];
      const right = level[index + 1];
      next.push(right === undefined ? left : parentOutput(left, right));
    }
    level = next;
  }
  return level[0].rootHex();
}

class Output {
  constructor(
    private readonly inputCv: Uint32Array,
    private readonly blockWords: Uint32Array,
    private readonly counter: number,
    private readonly blockLength: number,
    private readonly flags: number,
  ) {}

  chainingValue(): Uint32Array {
    return compress(
      this.inputCv,
      this.blockWords,
      this.counter,
      this.blockLength,
      this.flags,
    ).slice(0, 8);
  }

  rootHex(): string {
    const words = compress(
      this.inputCv,
      this.blockWords,
      0,
      this.blockLength,
      this.flags | ROOT,
    );
    let result = "";
    for (let index = 0; index < 8; index += 1) {
      result += wordHex(words[index]);
    }
    return result;
  }
}

function chunkOutput(bytes: Uint8Array, counter: number): Output {
  let cv = IV.slice();
  const blockCount = Math.max(1, Math.ceil(bytes.length / BLOCK_LENGTH));
  for (let block = 0; block < blockCount - 1; block += 1) {
    const start = block * BLOCK_LENGTH;
    const words = wordsFromBlock(bytes.subarray(start, start + BLOCK_LENGTH));
    const flags = block === 0 ? CHUNK_START : 0;
    cv = compress(cv, words, counter, BLOCK_LENGTH, flags).slice(0, 8);
  }

  const start = (blockCount - 1) * BLOCK_LENGTH;
  const finalBytes = bytes.subarray(start, start + BLOCK_LENGTH);
  return new Output(
    cv,
    wordsFromBlock(finalBytes),
    counter,
    finalBytes.length,
    CHUNK_END | (blockCount === 1 ? CHUNK_START : 0),
  );
}

function parentOutput(left: Output, right: Output): Output {
  const blockWords = new Uint32Array(16);
  blockWords.set(left.chainingValue(), 0);
  blockWords.set(right.chainingValue(), 8);
  return new Output(IV.slice(), blockWords, 0, BLOCK_LENGTH, PARENT);
}

function wordsFromBlock(bytes: Uint8Array): Uint32Array {
  const words = new Uint32Array(16);
  for (let index = 0; index < bytes.length; index += 1) {
    words[index >>> 2] |= bytes[index] << ((index & 3) * 8);
  }
  return words;
}

function compress(
  inputCv: Uint32Array,
  blockWords: Uint32Array,
  counter: number,
  blockLength: number,
  flags: number,
): Uint32Array {
  const state = new Uint32Array(16);
  state.set(inputCv, 0);
  state.set(IV.subarray(0, 4), 8);
  state[12] = counter >>> 0;
  state[13] = Math.floor(counter / 0x1_0000_0000) >>> 0;
  state[14] = blockLength;
  state[15] = flags;

  let message = blockWords.slice();
  for (let round = 0; round < 7; round += 1) {
    g(state, 0, 4, 8, 12, message[0], message[1]);
    g(state, 1, 5, 9, 13, message[2], message[3]);
    g(state, 2, 6, 10, 14, message[4], message[5]);
    g(state, 3, 7, 11, 15, message[6], message[7]);
    g(state, 0, 5, 10, 15, message[8], message[9]);
    g(state, 1, 6, 11, 12, message[10], message[11]);
    g(state, 2, 7, 8, 13, message[12], message[13]);
    g(state, 3, 4, 9, 14, message[14], message[15]);
    if (round < 6) message = new Uint32Array(permute(message));
  }

  const output = new Uint32Array(16);
  for (let index = 0; index < 8; index += 1) {
    output[index] = state[index] ^ state[index + 8];
    output[index + 8] = state[index + 8] ^ inputCv[index];
  }
  return output;
}

function g(
  state: Uint32Array,
  a: number,
  b: number,
  c: number,
  d: number,
  mx: number,
  my: number,
): void {
  state[a] = (state[a] + state[b] + mx) >>> 0;
  state[d] = rotateRight(state[d] ^ state[a], 16);
  state[c] = (state[c] + state[d]) >>> 0;
  state[b] = rotateRight(state[b] ^ state[c], 12);
  state[a] = (state[a] + state[b] + my) >>> 0;
  state[d] = rotateRight(state[d] ^ state[a], 8);
  state[c] = (state[c] + state[d]) >>> 0;
  state[b] = rotateRight(state[b] ^ state[c], 7);
}

function permute(message: Uint32Array): Uint32Array {
  const result = new Uint32Array(16);
  for (let index = 0; index < 16; index += 1) {
    result[index] = message[MESSAGE_PERMUTATION[index]];
  }
  return result;
}

function rotateRight(value: number, amount: number): number {
  return ((value >>> amount) | (value << (32 - amount))) >>> 0;
}

function wordHex(value: number): string {
  let result = "";
  for (let byte = 0; byte < 4; byte += 1) {
    result += (value >>> (byte * 8) & 0xff).toString(16).padStart(2, "0");
  }
  return result;
}
