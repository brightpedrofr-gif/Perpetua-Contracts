import { describe, expect, it, vi } from 'vitest';
import { createElement, type ReactNode } from 'react';
import TestRenderer, { act } from 'react-test-renderer';
import { PerpetuaContext } from './usePerpetua.js';
import { useStream, type UseStreamOptions, type UseStreamResult } from './useStream.js';
import type { Stream } from './types.js';

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const mk = (id: bigint): Stream => ({ id }) as Stream;

async function mount(client: any, id: bigint, opts: UseStreamOptions = {}) {
  const ref: { current: UseStreamResult | null } = { current: null };
  const C = () => {
    ref.current = useStream(id, opts);
    return null;
  };
  let r!: TestRenderer.ReactTestRenderer;
  await act(async () => {
    r = TestRenderer.create(
      createElement(PerpetuaContext.Provider, { value: client } as any, createElement(C) as ReactNode),
    );
  });
  return { ref, r };
}

describe('useStream', () => {
  it('throws without a provider', () => {
    const C = () => { useStream(1n); return null; };
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(() => TestRenderer.create(createElement(C))).toThrow(/PerpetuaProvider/);
    spy.mockRestore();
  });

  it('loads the stream on mount', async () => {
    const client = { getStream: vi.fn().mockResolvedValue(mk(1n)) };
    const { ref } = await mount(client, 1n);
    expect(ref.current!.stream?.id).toBe(1n);
    expect(ref.current!.loading).toBe(false);
    expect(ref.current!.error).toBeNull();
  });

  it('wraps Error and non-Error failures, then clears on success', async () => {
    const client = { getStream: vi.fn().mockRejectedValueOnce('boom').mockResolvedValue(mk(2n)) };
    const { ref } = await mount(client, 2n);
    expect(ref.current!.error?.message).toBe('boom');
    await act(async () => { await ref.current!.refresh(); });
    expect(ref.current!.error).toBeNull();
    expect(ref.current!.stream?.id).toBe(2n);

    const c2 = { getStream: vi.fn().mockRejectedValue(new Error('rpc')) };
    const m2 = await mount(c2, 3n);
    expect(m2.ref.current!.error?.message).toBe('rpc');
  });

  it('does not fetch when disabled', async () => {
    const client = { getStream: vi.fn().mockResolvedValue(mk(1n)) };
    const { ref } = await mount(client, 1n, { enabled: false });
    expect(client.getStream).not.toHaveBeenCalled();
    expect(ref.current!.loading).toBe(false);
  });

  it('does not stack concurrent fetches', async () => {
    let resolve!: (s: Stream) => void;
    const client = { getStream: vi.fn(() => new Promise<Stream>((r) => { resolve = r; })) };
    const { ref } = await mount(client, 1n);
    await act(async () => { void ref.current!.refresh(); });
    expect(client.getStream).toHaveBeenCalledTimes(1);
    await act(async () => { resolve(mk(1n)); });
  });

  it('polls on the interval and ignores non-positive intervals', async () => {
    vi.useFakeTimers();
    try {
      const client = { getStream: vi.fn().mockResolvedValue(mk(1n)) };
      await mount(client, 1n, { refreshIntervalMs: 1000 });
      await act(async () => { await vi.advanceTimersByTimeAsync(2100); });
      expect(client.getStream.mock.calls.length).toBeGreaterThanOrEqual(3);
      const c0 = { getStream: vi.fn().mockResolvedValue(mk(1n)) };
      await mount(c0, 1n, { refreshIntervalMs: 0 });
      await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
      expect(c0.getStream).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('drops results after unmount', async () => {
    let resolve!: (s: Stream) => void;
    const client = { getStream: vi.fn(() => new Promise<Stream>((r) => { resolve = r; })) };
    const { ref, r } = await mount(client, 1n);
    await act(async () => { r.unmount(); resolve(mk(1n)); });
    expect(ref.current!.stream).toBeNull();
  });
});
