import type { HostToolName } from "./types.ts";

/**
 * Common tool declarations are intentionally adapter-only.  The daemon binds
 * the actual workspace, shell, artifact, and terminal-command operations;
 * this package never opens a repository or invents authority.
 */
export interface CommonToolAdapter {
  readonly name: Extract<
    HostToolName,
    | "workspace_read"
    | "workspace_write"
    | "workspace_edit"
    | "workspace_search"
    | "workspace_list"
    | "shell"
    | "artifact_seal"
    | "artifact_read"
    | "product_submit_ticket"
    | "candidate_checkpoint_regression"
    | "candidate_submit"
    | "quality_run_full_suite"
    | "quality_submit_review"
    | "work_complete"
  >;
  readonly description: string;
  readonly input_schema: Readonly<Record<string, unknown>>;
  readonly invoke: (input: unknown) => Promise<unknown>;
}

export type CommonToolImplementation = {
  readonly [K in CommonToolAdapter["name"]]: (input: unknown) => Promise<unknown>;
};

export function createCommonToolAdapters(
  implementations: Partial<CommonToolImplementation>,
): readonly CommonToolAdapter[] {
  const declarations: readonly [CommonToolAdapter["name"], string][] = [
    ["workspace_read", "Read exact bytes beneath the assigned workspace."],
    ["workspace_write", "Write beneath the assigned workspace."],
    ["workspace_edit", "Apply an edit beneath the assigned workspace."],
    ["workspace_search", "Search the assigned workspace."],
    ["workspace_list", "List the assigned workspace."],
    ["shell", "Run one assigned shell command in the workspace."],
    ["artifact_seal", "Seal one approved staging file through the daemon."],
    ["artifact_read", "Read one allowed sealed assignment evidence artifact."],
    ["product_submit_ticket", "Submit a Product ticket proposal."],
    ["candidate_checkpoint_regression", "Submit an Engineering regression checkpoint."],
    ["candidate_submit", "Submit an Engineering candidate tree."],
    ["quality_run_full_suite", "Run the application-owned full suite."],
    ["quality_submit_review", "Submit a Quality review."],
    ["work_complete", "Submit the one assignment terminal result."],
  ];
  return declarations.flatMap(([name, description]) => {
    const invoke = implementations[name];
    if (invoke === undefined) return [];
    return [{
      name,
      description,
      input_schema: { type: "object", additionalProperties: false },
      invoke,
    }];
  });
}
