import { buildIndex, runFilter, type FilterIndex, type FilterResult, type FilterSpec, type FilterTree } from "./filter";
import type { WorkerIn, WorkerOut } from "./filter.worker";

/** Above this many entries the filter runs in a Web Worker so typing never waits for it. */
export const WORKER_THRESHOLD = 20_000;

/**
 * Runs filters over one listing. Small listings are filtered on the spot; big ones go to a worker
 * that keeps its own copy of the names. Callers ignore results that a newer `run` has superseded.
 */
export class FilterRunner {
  private index: FilterIndex | null = null;
  private worker: Worker | null = null;
  private seq = 0;
  private waiting = new Map<number, { spec: FilterSpec; resolve: (r: FilterResult) => void }>();

  constructor(private readonly tree: FilterTree) {
    if (tree.names.length > WORKER_THRESHOLD && typeof Worker !== "undefined") {
      try {
        const w = new Worker(new URL("./filter.worker.ts", import.meta.url), { type: "module" });
        w.onmessage = (e: MessageEvent<WorkerOut>) => {
          const done = this.waiting.get(e.data.id);
          this.waiting.delete(e.data.id);
          done?.resolve(e.data.result);
        };
        w.onerror = () => this.fallBack();
        w.postMessage({ type: "load", tree } satisfies WorkerIn);
        this.worker = w;
      } catch {
        this.worker = null;
      }
    }
  }

  /** The worker died: answer what is waiting on this thread, and do the rest here from now on. */
  private fallBack() {
    this.worker?.terminate();
    this.worker = null;
    const pending = [...this.waiting.values()];
    this.waiting.clear();
    for (const p of pending) p.resolve(this.runHere(p.spec));
  }

  private runHere(spec: FilterSpec): FilterResult {
    this.index ??= buildIndex(this.tree);
    return runFilter(this.index, spec);
  }

  run(spec: FilterSpec): Promise<FilterResult> {
    const w = this.worker;
    if (w) {
      const id = ++this.seq;
      return new Promise((resolve) => {
        this.waiting.set(id, { spec, resolve });
        w.postMessage({ type: "run", id, spec } satisfies WorkerIn);
      });
    }
    return Promise.resolve(this.runHere(spec));
  }

  dispose() {
    this.worker?.terminate();
    this.worker = null;
    this.waiting.clear();
  }
}
