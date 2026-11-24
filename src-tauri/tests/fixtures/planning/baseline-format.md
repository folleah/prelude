# prelude.yaml and response format

currentPreludeYaml is YAML with schemaVersion: 1, plan, and assumptions. The local file can also store project title, description, chat, and settings. Those fields are managed by Prelude and are outside this editing interface.

- plan.harness is a single selection, with id, version, and reason. Its id can be empty in a new project.
- plan.components is an ordered array of selections with the same fields. Entries may be models, skills, tools, plugins, or infrastructure. Catalog ID identifies a component. Prelude fills version from the catalog when adding one.
- plan.roles is an ordered array of objects with name, responsibility, interaction, reason, and optional modelId and parallelWith. parallelWith is an array of other role names that can work concurrently. Each role name is unique. modelId references a model already in plan.components.
- plan.instructions and assumptions are arrays of strings.

Return one plain YAML document in block style, without Markdown fences, JSON, an entire plan, or commentary. The document has an operations list, an optional assumptions list and an optional projectBrief string. Omit assumptions unless the request changes them. If provided, assumptions replaces the existing list. Quote strings that contain YAML punctuation.

For an initial plan, include projectBrief as a YAML block string (use |). Turn the idea, the request, any discoveryAnswers (each pair is a question and its answer), and relevant observed project facts into a coherent standalone description. Write in the language of the project brief or request. Explain the goal, its intended users when known, the core scenarios and scope, and the requirements and constraints that matter to development. Use concise prose and Markdown lists where useful. Preserve user decisions, combine overlapping details, and omit irrelevant or unknown categories. Label material assumptions and open decisions explicitly. Do not reproduce the interview or present inferred features as requirements. Do not include credentials or machine-local paths. On later turns, treat idea as the existing brief and return projectBrief only when the project scope or requirements change or when the old brief still contains a raw interview. Tool, role and dependency changes alone do not require a new brief.

    operations:
      - op: addComponent
        id: nginx
        reason: "Routes incoming requests to the web service."

Allowed operations and their fields:

hookChecks is a separate top-level list of small, repeatable instructions, one action per hook. Use a short imperative title such as "Run tests". In the description, use one short imperative sentence that says what to run and when, such as "After changing source code, run the project's fast tests." Write both fields in moduleDescriptionLanguage and describe the project action without naming the agent, model, or export mechanism. Name a project tool when it helps identify the check.

When starting from an empty plan, always return hookChecks. For software projects, propose running fast tests whenever appropriate for the selected stack, even if this is a new project whose test command will be configured later or the current export target cannot generate the hook. Choose other quick checks only when they add value. Use [] when no useful automatic action fits the project.

A proposal can also carry an optional check field with the value test, lint, or typecheck. Use it at most once per value and only for a matching non-interactive project script suitable after each completed turn, with npm, pnpm, Yarn, or Bun. Prelude uses this field for executable configurations in supported export targets. Omit check for other proposals. Do not invent existing scripts, promise automatic execution, or produce shell commands. Leave slow, interactive, or external-service checks as manual instructions. Avoid duplicating executable checks in plan instructions.

On later turns omit hookChecks to preserve it, or provide its complete new list. Use [] to remove hooks. If currentHookChecks contains legacy entries with missing titles or descriptions, supply the complete list with concise instructions even when the hook selection stays the same.

- setHarness or addComponent: id, optional reason.
- removeComponent: id.
- addRole: role containing name, responsibility, interaction, reason, optional modelId and parallelWith.
- updateRole: name identifies the role. Include only fields to change from newName, responsibility, interaction, reason, modelId, and parallelWith.
- removeRole: name.
- addInstruction or removeInstruction: text. For removal, use the exact existing instruction.

Use an empty quoted string for modelId to clear its assignment and an empty list for parallelWith to clear its relationships. When no changes are needed, return operations: []. Use exact camelCase operation and field names. Write new component and harness reasons in moduleDescriptionLanguage. Preserve existing instruction text and use English for other new plan text unless the user asks for another language.
