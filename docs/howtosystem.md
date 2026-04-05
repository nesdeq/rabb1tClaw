# How to Write Optimal System Prompts

Best practices synthesized from Anthropic, OpenAI, Google, Meta, and academic research (2024-2026).

---

## 1. Structure

Order sections by descending authority. The model treats what comes first as foundational context and what comes last as the immediate task.

**Recommended order:**
1. **Role** — Who the model is, personality, domain expertise
2. **Behavioral rules** — Hard constraints, always/never lists
3. **Tools and capabilities** — What's available, invocation format
4. **Output format** — Structure, length, style contract
5. **Examples** — Placed alongside the rules they demonstrate
6. **Context and state** — Dynamic injections (memory, task logs)

**Delimiters:** Use one format consistently — never mix XML and Markdown in the same prompt. Markdown headers (`##`) are the default for most models. XML tags (`<instructions>`) are preferred for Claude when mixing instructions with large context blocks.

*Sources: OpenAI GPT-4.1 Guide, Anthropic Claude 4 Best Practices, Google Gemini 3 Guide*

---

## 2. Role Definition

### Be specific, not generic
- Bad: "You are a helpful assistant."
- Good: "You are Artemis, a voice assistant on a Rabbit R1. Every word is spoken aloud via TTS."

### Provide motivation behind rules
Explain WHY, not just WHAT. The model generalizes from explanations better than it memorizes bare rules.

- Bad: "Never use ellipses."
- Good: "Your output is read by TTS — ellipses produce awkward pauses."

### Bound responsibilities explicitly
State what the model SHOULD do and what it should NOT attempt. Unbounded roles produce unpredictable behavior.

*Sources: Anthropic Claude 4 Best Practices, OpenAI GPT-5 Guide*

---

## 3. Instructions

### Positive over negative framing
Tell the model what TO DO, not what NOT to do. Negative constraints are weaker and less reliably followed.

- Bad: "Do not use markdown."
- Good: "Write in plain flowing prose."

### Use "always" and "never" for hard constraints
Reserve these for rules that genuinely have zero exceptions. Overusing them dilutes their force.

### Avoid over-prompting
Claude 4.5/4.6 and GPT-5 are more responsive to system prompts than earlier generations. Where you once needed "CRITICAL: You MUST...", now "Use X when..." suffices. Aggressive emphasis causes overtriggering.

### If/then for conditional behavior
Edge cases are best handled with explicit conditional logic rather than hoping the model infers the right behavior.

### Control overeagerness
2025-2026 models tend to overengineer, add unsolicited features, and over-explore when given thoroughness language. Counter this with explicit scope discipline:
- "Match the user's request — not an idealized version of it"
- "One request, one action — do not add extras"
- Set tool-call budgets or iteration limits in the prompt itself

### Persona vs. instructions
Models take assigned personas seriously and may prioritize persona adherence over instructions. If a persona conflicts with a rule, the persona often wins. Keep personas lean and ensure they don't contradict behavioral rules.

*Sources: Anthropic Claude 4 Best Practices, OpenAI GPT-4.1 Guide, Google Gemini 3 Guide*

---

## 4. Examples

### Quantity
3-5 examples is the sweet spot. Fewer may be insufficient; more risks overfitting to example patterns.

**Exception:** Reasoning models (o-series, DeepSeek R1) perform worse with few-shot examples. Use zero-shot for these models.

### Placement
Place examples alongside the rules they demonstrate, not in a disconnected section. This reinforces the rule immediately.

### Design
- Show desired behavior (patterns), not undesired behavior (anti-patterns)
- Make examples relevant to actual use cases
- Cover edge cases and diverse scenarios
- Maintain identical formatting across all examples

*Sources: Anthropic Claude 4 Best Practices, OpenAI GPT-4.1 Guide, Google Gemini Prompting Strategies*

---

## 5. Output Format

### Define the contract explicitly
Specify structure, length, tone, and required sections. The model cannot reliably infer format expectations from examples alone.

### Match prompt style to desired output
The model mirrors the formatting it sees in the prompt. If your prompt uses markdown, the output will use markdown. If your prompt is plain prose, the output trends toward plain prose.

### Length constraints
Explicit length bounds are one of the highest-impact controls. "Under 60 seconds of speech" or "200-500 words" are more effective than "be concise."

### Verbosity as a first-class control
Modern models vary dramatically in default verbosity. Claude 4.6 is concise by default; Gemini 3 is terse; GPT-5 adds a verbosity parameter. Always specify your length/verbosity expectation explicitly rather than relying on model defaults.

*Sources: Anthropic Claude 4 Best Practices, OpenAI GPT-5 Guide*

---

## 6. Multi-Agent Prompts

### Each agent gets a tightly focused prompt
Define expertise, boundaries, input format, and output contract. A specialist agent should not need to understand the full system.

### Orchestrator prompts need three elements
OpenAI found ~20% improvement from including:
1. **Persistence** — "Continue until task completion before yielding"
2. **Tool grounding** — "Use tools; do not guess or hallucinate"
3. **Planning** — "Reflect between actions"

### Self-contained task descriptions
Sub-agents have no conversation context. Every delegation must include all necessary information. This is the single most common source of bugs in multi-agent systems.

### Consistent terminology across agents
If the main agent calls it a "workspace," every sub-agent must also call it "workspace." Inconsistent naming causes confusion and errors.

*Sources: OpenAI GPT-4.1 Guide, Anthropic Context Engineering, Google Multi-Agent Patterns*

---

## 7. Token Efficiency

### Every token must earn its place
Context is a finite resource with diminishing returns. Context rot (accuracy decreasing with length) is real. Long system prompts degrade instruction following.

### Techniques
- Replace verbose phrases with shorter equivalents ("In order to" → "To")
- Include only context relevant to the current task
- Use precise instructions instead of verbose explanations
- Constrain output length explicitly — a 400-token limit vs 1500 saves 76% cost

### Quality first, optimize later
Build for correctness with detailed prompts, then iteratively trim without losing behavior.

*Sources: Anthropic Context Engineering, Portkey Token Efficiency Guide*

---

## 8. Robustness

### Assume user input is noisy
Speech-to-text, typos, fragments, missing punctuation. The prompt should instruct generous interpretation.

### Handle uncertainty explicitly
State what the model should do when it lacks information: ask, refuse, or proceed with caveats.

### Test adversarially
Prompts that work for friendly inputs may fail under edge cases. Test with ambiguous, contradictory, and adversarial inputs.

### Don't rely solely on instruction hierarchy for safety
Research (2025) shows models tend to follow later-appearing instructions regardless of declared priority. Use defense-in-depth: structural separation, input sanitization, output validation.

### Ground the model in provided context
For agents that process external data (search results, documents): instruct the model to treat provided context as the limit of truth. "Use only the provided search results. If information is missing, say so." This reduces hallucination significantly.

*Sources: "Control Illusion" paper (2025), OWASP LLM01:2025, Google Prompt Injection Defenses, Gemini 3 Guide*

---

## 9. Mechanical Parsing Compatibility

When system prompts define formats that code will parse:

### Document the exact syntax
Show the precise format with a concrete example. "Fenced code blocks with the type `code`" is ambiguous. "Exactly ` ```code ` on its own line" is unambiguous.

### Match the parsing code
The prompt's format specification must exactly match what the parser expects. If the code looks for `### Packages` followed by a fenced block, the prompt must show exactly that.

### Test the contract
Every format example in the prompt should be a valid input to the parsing code.

---

## 10. Consistency Across a Prompt Suite

When multiple prompts form a system:

### Shared terminology
Define terms once and use them identically everywhere. If the sandbox path is `/workspace/`, every prompt that references it must use `/workspace/`.

### Shared formatting conventions
If one prompt uses `##` headers, all should. If one uses bullet lists, all should.

### Cross-references must be accurate
If the main prompt says "the code agent produces a `### Code` section," the code agent's prompt must actually produce that section.

### Template variables
Use consistent placeholder syntax (`{variable_name}`) and ensure every placeholder is populated by the code at runtime.
