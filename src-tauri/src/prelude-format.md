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

Optional hookChecks is a separate top-level list with values test, lint, and typecheck. Prelude can export these as repository-scoped Codex Stop hooks for npm, pnpm, Yarn, or Bun scripts of the same name. On initial planning, choose only checks that are appropriate to run after each completed turn and supported by the selected package manager. Use repository evidence and the user's preferences. Leave slow, interactive, or external-service checks as manual instructions. Do not invent existing scripts or produce shell commands for hooks. On later turns omit hookChecks to preserve the current list, or supply its new complete list, including [] to remove hooks. Do not duplicate routine hooked checks in plan instructions.

- setHarness or addComponent: id, optional reason.
- removeComponent: id.
- addRole: role containing name, responsibility, interaction, reason, optional modelId and parallelWith.
- updateRole: name identifies the role. Include only fields to change from newName, responsibility, interaction, reason, modelId, and parallelWith.
- removeRole: name.
- addInstruction or removeInstruction: text. For removal, use the exact existing instruction.

Use an empty quoted string for modelId to clear its assignment and an empty list for parallelWith to clear its relationships. When no changes are needed, return operations: []. Use exact camelCase operation and field names. Write new component and harness reasons in moduleDescriptionLanguage. Preserve existing instruction text and use English for other new plan text unless the user asks for another language.
