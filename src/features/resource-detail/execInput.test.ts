import { expect, test } from "bun:test";
import { createExecInput, EXEC_INPUT_CHUNK_BYTES, EXEC_PENDING_INPUT_BYTES } from "./execInput";

test("waits for acknowledgement and preserves UTF-8 and input order", async () => {
	const writes: string[] = [];
	const first = Promise.withResolvers<boolean>();
	const done = Promise.withResolvers<void>();
	const errors: unknown[] = [];
	const input = createExecInput(async (data) => {
		writes.push(data);
		if (writes.length === 1) return first.promise;
		if (data === "next") done.resolve();
		return true;
	}, (error) => errors.push(error));
	const text = `${"x".repeat(EXEC_INPUT_CHUNK_BYTES - 1)}😀end`;
	input.enqueue(text);
	input.enqueue("next");
	expect(writes).toEqual(["x".repeat(EXEC_INPUT_CHUNK_BYTES - 1)]);
	first.resolve(true);
	await done.promise;
	expect(writes.join("")).toBe(`${text}next`);
	expect(writes.every((data) => new TextEncoder().encode(data).length <= EXEC_INPUT_CHUNK_BYTES)).toBe(true);
	expect(errors).toEqual([]);
});

test("rejects a whole input when queued plus in-flight bytes exceed the budget", () => {
	const writes: string[] = [];
	const errors: unknown[] = [];
	const input = createExecInput((data) => {
		writes.push(data);
		return new Promise<boolean>(() => {});
	}, (error) => errors.push(error));
	input.enqueue("x".repeat(EXEC_PENDING_INPUT_BYTES));
	input.enqueue("extra");
	expect(errors.length).toBe(1);
	expect(writes.length).toBe(1);
	input.dispose();
});

test("disposal prevents queued input from reaching another session", async () => {
	const pending = Promise.withResolvers<boolean>();
	const writes: string[] = [];
	const input = createExecInput((data) => { writes.push(data); return pending.promise; }, () => {});
	input.enqueue("first");
	input.enqueue("second");
	input.dispose();
	pending.resolve(true);
	await pending.promise;
	await Promise.resolve();
	input.enqueue("third");
	expect(writes).toEqual(["first"]);
});

test("a failed write reports the error and stops pending input", async () => {
	const failure = new Error("closed");
	const reported = Promise.withResolvers<unknown>();
	const writes: string[] = [];
	const input = createExecInput(async (data) => { writes.push(data); throw failure; }, reported.resolve);
	input.enqueue("first");
	input.enqueue("second");
	expect(await reported.promise).toBe(failure);
	input.enqueue("third");
	expect(writes).toEqual(["first"]);
});
