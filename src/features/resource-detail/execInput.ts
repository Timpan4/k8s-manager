import { messageFromError } from "@/lib/error-redaction";
import type { AppError } from "@/lib/types";

// Match the approved backend limits. Pending bytes include the in-flight write.
export const EXEC_INPUT_CHUNK_BYTES = 16 * 1024;
export const EXEC_PENDING_INPUT_BYTES = 256 * 1024;

function utf8Bytes(character: string): number {
	const point = character.codePointAt(0) ?? 0;
	return point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
}

function hasAppErrorKind<Value>(value: Value): value is Value & Pick<AppError, "kind"> {
	return value instanceof Object && "kind" in value && String(value.kind) === value.kind;
}

function parseExecInputError(cause: unknown): AppError {
	const kind = hasAppErrorKind(cause) ? cause.kind : "session";
	return { kind, message: messageFromError(cause) };
}

export function createExecInput(
	write: (data: string) => Promise<boolean>,
	onError: (error: AppError) => void,
) {
	let pending = 0;
	let running = false;
	let disposed = false;
	const queue: string[] = [];

	async function flush() {
		if (running || disposed) return;
		running = true;
		try {
			while (queue.length > 0 && !disposed) {
				const data = queue.shift();
				if (data === undefined) break;
				let start = 0;
				while (start < data.length && !disposed) {
					let end = start;
					let bytes = 0;
					for (const character of data.slice(start)) {
						const size = utf8Bytes(character);
						if (bytes + size > EXEC_INPUT_CHUNK_BYTES) break;
						bytes += size;
						end += character.length;
					}
					if (!(await write(data.slice(start, end)))) {
						throw new Error("Exec input was not accepted");
					}
					if (disposed) return;
					pending -= bytes;
					start = end;
				}
			}
		} catch (error) {
			if (!disposed) {
				disposed = true;
				queue.length = 0;
				pending = 0;
				onError(parseExecInputError(error));
			}
		} finally {
			running = false;
		}
	}

	return {
		enqueue(data: string) {
			if (disposed || data.length === 0) return;
			let bytes = 0;
			for (const character of data) {
				bytes += utf8Bytes(character);
				if (bytes > EXEC_PENDING_INPUT_BYTES - pending) {
					onError({ kind: "session", message: "Terminal input exceeds the 256 KiB pending limit; this input was not sent" });
					return;
				}
			}
			pending += bytes;
			queue.push(data);
			void flush();
		},
		dispose() {
			disposed = true;
			queue.length = 0;
			pending = 0;
		},
	};
}
