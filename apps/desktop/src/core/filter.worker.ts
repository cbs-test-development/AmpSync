import { buildIndex, runFilter, type FilterIndex, type FilterResult, type FilterSpec, type FilterTree } from "./filter";

/** Messages: `load` once per listing, then one `run` per change of the filter. */
export type WorkerIn = { type: "load"; tree: FilterTree } | { type: "run"; id: number; spec: FilterSpec };
export interface WorkerOut {
  id: number;
  result: FilterResult;
}

const ctx = self as unknown as {
  onmessage: ((e: MessageEvent<WorkerIn>) => void) | null;
  postMessage(message: WorkerOut, options: { transfer: Transferable[] }): void;
};

let index: FilterIndex | null = null;

ctx.onmessage = (e) => {
  const m = e.data;
  if (m.type === "load") {
    index = buildIndex(m.tree);
  } else if (index) {
    const r = runFilter(index, m.spec);
    ctx.postMessage({ id: m.id, result: r }, { transfer: [r.match.buffer, r.visible.buffer, r.files.buffer, r.bytes.buffer] });
  }
};
