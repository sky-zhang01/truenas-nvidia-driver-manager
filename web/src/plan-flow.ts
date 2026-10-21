import { postPlan, type PlanSource, type PlanReady, type PlanBlocked, type PlanUncomputable } from "./api.js";

interface PlanFlow {
  source: PlanSource;
  signal?: AbortSignal;
  owns: () => boolean;
  ready: (result: PlanReady) => void;
  blocked: (result: PlanBlocked) => void;
  uncomputable: (result: PlanUncomputable) => void;
  failed: (error: unknown) => void | Promise<void>;
  orphanedReady?: (planId: string) => void;
  finish: () => void;
}

/** Each caller retains its cancellation, ownership and refresh rules around one plan request. */
export async function runPlanFlow(flow: PlanFlow): Promise<void> {
  try {
    const result = await postPlan(flow.source, flow.signal);
    if (!flow.owns()) {
      if (result.result === "ready") flow.orphanedReady?.(result.plan_id);
      return;
    }
    if (result.result === "ready") flow.ready(result);
    else if (result.result === "blocked") flow.blocked(result);
    else flow.uncomputable(result);
  } catch (error) {
    if (flow.owns()) await flow.failed(error);
  } finally {
    flow.finish();
  }
}
