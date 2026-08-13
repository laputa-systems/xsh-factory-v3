import { dirname, join } from "@std/path";
import type { AuthorityAdmissionFrame, NormalizedSessionSummary } from "./types.ts";

const encoder = new TextEncoder();

/** Bounds the daemon-attested packet before any JSON or prompt decoding. */
export const MAX_ASSIGNMENT_PACKET_BYTES = 3 * 1024 * 1024 - 64 * 1024;

/** Bounds the one startup line to the local protocol's 4 MiB response ceiling. */
export const MAX_ADMISSION_FRAME_BYTES = (4 * 1024 * 1024) - 1;

function jsonLine(value: unknown): Uint8Array {
  try {
    return encoder.encode(
      JSON.stringify(value, (_key, item) => typeof item === "bigint" ? `${item}n` : item) + "\n",
    );
  } catch (error) {
    throw new Error(
      `SDK event is not JSON serializable: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

/** Appends every raw SDK event immediately to the assignment-local NDJSON stream. */
export class TranscriptWriter {
  readonly path: string;
  #file: Deno.FsFile | undefined;
  #sequence = 0;
  #writesSinceSync = 0;
  #writtenBytes = 0;
  #truncated = false;
  readonly #byteLimit: number;

  private constructor(path: string, file: Deno.FsFile, byteLimit: number) {
    this.path = path;
    this.#file = file;
    this.#byteLimit = byteLimit;
  }

  static async open(path: string, byteLimit: number): Promise<TranscriptWriter> {
    if (!Number.isSafeInteger(byteLimit) || byteLimit < 1) {
      throw new Error("transcript byte limit is invalid");
    }
    await Deno.mkdir(dirname(path), { recursive: true });
    return new TranscriptWriter(
      path,
      await Deno.open(path, { create: true, truncate: true, write: true }),
      byteLimit,
    );
  }

  /**
   * Retains ordered raw SDK events only while their exact NDJSON encoding
   * fits the assigned bound. Pi may emit cumulative snapshots that dwarf the
   * streamed text delta, so model-output accounting alone cannot safely cap
   * this durable artifact.
   */
  async append(event: unknown): Promise<{ readonly truncated: boolean }> {
    if (this.#file === undefined) throw new Error("transcript is closed");
    if (this.#truncated) return { truncated: true };
    const bytes = jsonLine({ sequence: this.#sequence++, event });
    if (this.#writtenBytes + bytes.byteLength > this.#byteLimit) {
      this.#truncated = true;
      const marker = jsonLine({
        sequence: this.#sequence++,
        event: {
          type: "factory.transcript_truncated.v1",
          omitted_event_byte_length: bytes.byteLength,
          retained_byte_limit: this.#byteLimit,
        },
      });
      if (this.#writtenBytes + marker.byteLength <= this.#byteLimit) {
        await writeAll(this.#file, marker);
        this.#writtenBytes += marker.byteLength;
      }
      return { truncated: true };
    }
    await writeAll(this.#file, bytes);
    this.#writtenBytes += bytes.byteLength;
    if (++this.#writesSinceSync >= 32) {
      await this.#file.sync();
      this.#writesSinceSync = 0;
    }
    return { truncated: false };
  }

  async close(): Promise<void> {
    const file = this.#file;
    this.#file = undefined;
    if (file !== undefined) await file.sync();
    file?.close();
  }
}

/** Compresses a complete or partial NDJSON stream with the Web gzip primitive. */
export async function gzipFile(sourcePath: string, destinationPath: string): Promise<void> {
  await Deno.mkdir(dirname(destinationPath), { recursive: true });
  const source = await Deno.open(sourcePath, { read: true });
  const destination = await Deno.open(destinationPath, {
    create: true,
    truncate: true,
    write: true,
  });
  try {
    const compressed = source.readable.pipeThrough(new CompressionStream("gzip"));
    await compressed.pipeTo(
      new WritableStream<Uint8Array>({
        write: async (chunk) => {
          await writeAll(destination, chunk);
        },
      }),
    );
    await destination.sync();
  } finally {
    safeClose(source);
    safeClose(destination);
  }
}

async function writeAll(file: Deno.FsFile, bytes: Uint8Array): Promise<void> {
  let offset = 0;
  while (offset < bytes.byteLength) {
    const written = await file.write(bytes.subarray(offset));
    if (written <= 0) throw new Error("transcript write made no progress");
    offset += written;
  }
}

function safeClose(file: Deno.FsFile): void {
  try {
    file.close();
  } catch (error) {
    if (!(error instanceof Deno.errors.BadResource)) throw error;
  }
}

export async function writeManifest(
  path: string,
  manifest: unknown,
): Promise<void> {
  await Deno.mkdir(dirname(path), { recursive: true });
  await Deno.writeTextFile(path, JSON.stringify(manifest) + "\n");
}

export function transcriptPaths(stagingRoot: string): {
  readonly ndjson: string;
  readonly gzip: string;
  readonly required_read_manifest: string;
} {
  return {
    ndjson: join(stagingRoot, "session.ndjson"),
    gzip: join(stagingRoot, "session.ndjson.gz"),
    required_read_manifest: join(stagingRoot, "required-read-manifest.json"),
  };
}

export function summaryForJson(summary: NormalizedSessionSummary): NormalizedSessionSummary {
  return summary;
}

/** Reads the one newline-delimited startup gate from inherited actor FD 0. */
export async function readSessionAdmissionFrame(
  file: Deno.FsFile,
  byteLimit = MAX_ADMISSION_FRAME_BYTES,
): Promise<AuthorityAdmissionFrame> {
  const bytes: number[] = [];
  const chunk = new Uint8Array(1);
  while (bytes.length < byteLimit) {
    const count = await file.read(chunk);
    if (count === null) throw new Error("daemon closed before session.admitted");
    if (chunk[0] === 0x0a) break;
    bytes.push(chunk[0]);
  }
  if (bytes.length >= byteLimit) throw new Error("session.admitted frame is oversized");
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(bytes)));
  } catch (error) {
    throw new Error(
      `invalid session.admitted frame: ${error instanceof Error ? error.message : error}`,
    );
  }
  if (value === null || typeof value !== "object") {
    throw new Error("session.admitted is not an object");
  }
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record).sort().join(",");
  if (
    keys !==
      "assignment_id,packet_b64,packet_digest,protocol_version,session_id,session_revision,type"
  ) {
    throw new Error("session.admitted has unknown or missing fields");
  }
  if (
    record.type !== "session.admitted" || record.protocol_version !== 1 ||
    typeof record.assignment_id !== "string" || typeof record.packet_digest !== "string" ||
    typeof record.packet_b64 !== "string" || record.packet_b64.length === 0 ||
    !Number.isSafeInteger(record.session_id) || (record.session_id as number) < 1 ||
    !Number.isSafeInteger(record.session_revision) || (record.session_revision as number) < 0 ||
    !/^[a-f0-9]{64}$/.test(record.packet_digest as string)
  ) throw new Error("session.admitted fields are invalid");
  return record as unknown as AuthorityAdmissionFrame;
}
