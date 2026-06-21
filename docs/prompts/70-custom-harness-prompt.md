# Agentic OS Harness Requirements and Subagent Design Prompt

You are building the harness layer for an agentic OS.

The OS already replaces the traditional userspace with long-running agent processes. It has:

- A full MCP gateway for tool access
- Existing tooling connections
- A TUI for user interaction and control
- An LLM gateway for routing model calls
- The ability to run persistent agent processes
- A knowledge base layer previously used as “GBrain”
- Prior personal-agent behavior inspired by OpenClaw

The goal is to design and implement a custom personal operating harness that takes over the user’s manual knowledge work: organizing, updating, searching, tracking, summarizing, and coordinating across tools.

The harness should function as a persistent personal operations layer across email, people, customer discussions, team discussions, product planning, Linear, Attio, GitHub, and the knowledge base.

## Core Objective

Build a harness that continuously coordinates between the user’s communication, planning, execution, and memory systems.

The harness should reduce or eliminate the need for the user to manually:

- Search across emails, notes, GitHub, Linear, Attio, and knowledge base records
- Track important people and relationship follow-ups
- Maintain focus lists of high-priority people, customers, investors, teammates, and partners
- Identify open items from conversations
- Summarize customer, team, and product discussions
- Update Linear issues, GitHub context, Attio records, and knowledge base entries
- Convert unstructured discussions into structured product planning artifacts
- Retrieve the right context before meetings, decisions, and follow-ups
- Keep a living memory of people, projects, customers, product decisions, and commitments

The harness should act like an always-on Chief of Staff, Product Ops lead, Knowledge Manager, and Execution Coordinator inside the agentic OS.

## Operating Model

The harness should be made of a central orchestrator and specialized subagents.

The central orchestrator is responsible for:

- Understanding the user’s current operating context
- Delegating tasks to the right subagents
- Maintaining global state across agents
- Resolving conflicts between tools and sources
- Deciding when to act autonomously and when to ask for approval
- Maintaining auditability of all actions
- Ensuring idempotent updates across external systems
- Routing tool calls through the MCP gateway
- Routing model calls through the LLM gateway
- Exposing state, actions, and pending approvals through the TUI

The harness must be persistent, event-driven, and context-aware. It should respond to new emails, calendar events, Linear updates, GitHub activity, Attio changes, knowledge-base changes, and user commands from the TUI.

## Required Subagents

Design and implement the following subagents.

### 1. Executive Orchestrator Agent

Purpose:  
The central coordinator for the entire harness.

Responsibilities:

- Maintain the user’s operating state
- Decide which subagent should handle each task
- Track active projects, people, customers, commitments, and open loops
- Decide when to summarize, update, search, escalate, or ask the user
- Maintain a daily, weekly, and project-level operating view
- Coordinate multi-step workflows across Gmail, Linear, Attio, GitHub, and GBrain
- Produce a unified “What matters now?” view

Key outputs:

- Daily operating brief
- Priority queue
- Pending approval queue
- Open-loop dashboard
- Cross-tool activity summary
- Recommended next actions

### 2. Inbox and Communications Agent

Purpose:  
Manage email and communication context.

Responsibilities:

- Scan incoming emails for priority, urgency, sender importance, and open items
- Identify emails requiring a response
- Detect commitments, asks, deadlines, and follow-ups
- Summarize important threads
- Connect email discussions to people, companies, projects, customers, Linear issues, GitHub repos, and knowledge-base records
- Draft responses when useful, but require approval before sending
- Tag, classify, archive, or organize emails according to user-approved rules

Key outputs:

- Important email summary
- Response-needed list
- Follow-up list
- Thread summaries
- Draft replies
- Extracted action items

### 3. People and Relationship Focus Agent

Purpose:  
Maintain the user’s important people focus list.

Responsibilities:

- Track important people across investors, customers, partners, teammates, advisors, prospects, and collaborators
- Maintain relationship context for each person
- Track last interaction, pending follow-ups, promised deliverables, next meeting, and relationship priority
- Detect when an important person appears in email, calendar, Attio, Linear, GitHub, or knowledge-base context
- Generate pre-meeting briefs and follow-up suggestions
- Recommend people the user should re-engage with

Key outputs:

- Important people focus list
- Relationship briefs
- Follow-up reminders
- Pre-meeting context
- Suggested outreach priorities

### 4. Open Items and Commitments Agent

Purpose:  
Track all open loops across the user’s work.

Responsibilities:

- Extract commitments, asks, deadlines, and unresolved questions from emails, meetings, notes, Linear, GitHub, and Attio
- Deduplicate open items across systems
- Track owner, status, due date, source, and related context
- Recommend where each open item should live: Linear, Attio, GitHub, knowledge base, or personal queue
- Close the loop when evidence shows the item has been completed
- Escalate stale or high-priority items

Key outputs:

- Unified open-items list
- Stale commitment report
- Newly detected asks
- Completion candidates
- Suggested updates to Linear, Attio, GitHub, and GBrain

### 5. Customer Context Agent

Purpose:  
Maintain structured memory of customer discussions and opportunities.

Responsibilities:

- Summarize customer conversations from email, notes, meetings, Attio, and related artifacts
- Track customer pain points, asks, objections, product feedback, timeline, stakeholders, next steps, and promised deliverables
- Update Attio records with structured summaries and next actions
- Link customer conversations to relevant product areas, Linear issues, GitHub repos, and roadmap items
- Generate customer briefs before calls
- Detect customer signals that should influence product planning

Key outputs:

- Customer account briefs
- Meeting summaries
- Product feedback extraction
- Attio update suggestions
- Customer-driven roadmap inputs
- Next-step recommendations

### 6. Team and Internal Discussion Agent

Purpose:  
Turn internal discussions into structured operating memory.

Responsibilities:

- Summarize internal team discussions
- Extract decisions, blockers, action items, owners, and unresolved questions
- Connect discussions to Linear projects, GitHub work, roadmap items, and knowledge-base entries
- Maintain a running state of major product and engineering conversations
- Detect when decisions conflict with prior context
- Generate internal updates for leadership or team review

Key outputs:

- Internal discussion summaries
- Decision logs
- Action-item lists
- Blocker summaries
- Team update drafts
- Knowledge-base updates

### 7. Product Planning Agent

Purpose:  
Translate discussions, customer inputs, and engineering work into product planning artifacts.

Responsibilities:

- Convert unstructured customer, team, and founder discussions into roadmap items
- Maintain product context across features, modules, releases, priorities, and open questions
- Update or propose Linear issues, projects, labels, and milestones
- Link GitHub activity to product roadmap context
- Maintain product decision records in GBrain
- Identify repeated customer asks and emerging product themes
- Keep planning artifacts clean, deduplicated, and current

Key outputs:

- Roadmap item drafts
- Linear issue proposals
- Product requirement summaries
- Decision records
- Product theme analysis
- Release planning context

### 8. Linear Operations Agent

Purpose:  
Keep Linear aligned with real product and engineering work.

Responsibilities:

- Search, create, update, link, and summarize Linear issues
- Detect when discussions imply a new issue, status update, priority change, or milestone change
- Keep Linear issues connected to customer context, GitHub work, and product planning docs
- Suggest updates before making them
- Require user approval for destructive or high-impact changes
- Maintain idempotency to avoid duplicate issues

Key outputs:

- Proposed Linear issue updates
- New issue drafts
- Project summaries
- Status-change suggestions
- Duplicate issue detection

### 9. Attio / CRM Agent

Purpose:  
Keep relationship and customer records current.

Responsibilities:

- Update company and person records with relevant context
- Track stakeholders, last touch, next steps, opportunity status, and customer asks
- Connect email and meeting context to Attio records
- Maintain customer/account summaries
- Identify stale relationships or opportunities needing follow-up
- Recommend CRM updates based on newly observed context

Key outputs:

- Attio update suggestions
- Company/person summaries
- Follow-up recommendations
- Opportunity notes
- Relationship status changes

### 10. GitHub Context Agent

Purpose:  
Connect code and engineering activity to product and planning context.

Responsibilities:

- Track relevant repos, PRs, issues, commits, discussions, and release activity
- Summarize GitHub work in product language
- Link GitHub activity to Linear issues, product roadmap items, customer asks, and GBrain records
- Detect when implementation work changes the product state
- Generate release notes and internal product updates from GitHub activity

Key outputs:

- GitHub activity summaries
- PR-to-product mappings
- Release note drafts
- Engineering progress summaries
- Suggested Linear and knowledge-base updates

### 11. GBrain / Knowledge Base Curator Agent

Purpose:  
Maintain the canonical working memory of the user and organization.

Responsibilities:

- Store durable context about people, companies, products, decisions, projects, commitments, and workflows
- Deduplicate and reconcile knowledge from email, Attio, Linear, GitHub, and discussions
- Maintain source links and provenance for every memory
- Distinguish between durable memory, temporary working context, and outdated information
- Create structured records for recurring entities and topics
- Retrieve the right context for the orchestrator and other subagents

Key outputs:

- Updated knowledge-base records
- Entity profiles
- Decision records
- Project memory
- Source-linked summaries
- Context packs for meetings and workflows

### 12. Search and Retrieval Agent

Purpose:  
Provide fast, cross-system retrieval.

Responsibilities:

- Search across email, knowledge base, Linear, GitHub, Attio, and available tool data
- Resolve ambiguous queries using context
- Return concise, source-linked answers
- Retrieve historical context for people, customers, projects, and decisions
- Support the TUI with interactive search and drill-down

Key outputs:

- Cross-tool search results
- Context packs
- Source-linked answers
- Entity timelines
- Related-record suggestions

### 13. Approval and Safety Agent

Purpose:  
Control autonomy, permissions, and user approval.

Responsibilities:

- Classify actions by risk level
- Require approval before sending emails, modifying external records, deleting, archiving, changing statuses, or creating high-impact artifacts
- Allow low-risk actions such as summarization, retrieval, draft creation, and internal note preparation to happen autonomously
- Maintain an approval queue in the TUI
- Log all actions and tool calls
- Enforce least-privilege tool access
- Support rollback where possible

Key outputs:

- Approval queue
- Risk classifications
- Action logs
- Rollback suggestions
- Policy violations or warnings

## Harness Requirements

The harness must support the following capabilities.

### 1. Persistent Memory

The system must maintain memory across sessions, tools, people, customers, and projects.

Memory should include:

- People
- Companies
- Projects
- Customers
- Product areas
- Roadmap items
- Open items
- Decisions
- Meeting summaries
- Email thread summaries
- Linear issues
- GitHub work
- Attio records
- User preferences
- Recurring workflows

Memory must include provenance and source links wherever possible.

### 2. Cross-Tool Context Graph

The harness should maintain an implicit or explicit context graph connecting:

- People
- Companies
- Emails
- Meetings
- Linear issues
- GitHub repos / PRs / issues
- Attio records
- Product areas
- Customer asks
- Roadmap items
- Knowledge-base entries
- Open items
- Decisions

The graph should allow the system to answer questions like:

- What do I owe this person?
- What happened with this customer?
- What product asks are recurring?
- What did we decide last time?
- What Linear issues relate to this conversation?
- What GitHub activity changed the product state?
- What should I focus on today?

### 3. Event-Driven Workflows

The harness should react to important events, including:

- New email from important person
- Email thread requiring response
- New meeting or upcoming meeting
- New customer discussion
- New Linear issue or status change
- New GitHub PR, issue, commit, or release
- New Attio update
- New knowledge-base entry
- User command from TUI

Each event should be classified, routed, summarized, linked to context, and, where appropriate, turned into an action or approval request.

### 4. TUI Control Surface

The TUI should expose:

- Today’s operating brief
- Important people focus list
- Open items
- Pending approvals
- Customer summaries
- Product planning queue
- Search interface
- Agent activity feed
- Tool-call logs
- Suggested actions
- Memory updates
- Errors and conflicts

The user should be able to approve, reject, edit, defer, or ask follow-up questions from the TUI.

### 5. Autonomy Levels

The harness should support clear autonomy levels.

Level 0: Read-only  
The system can search, summarize, and retrieve.

Level 1: Draft-only  
The system can create drafts, proposed updates, and suggested actions.

Level 2: Approved execution  
The system can execute actions only after explicit user approval.

Level 3: Trusted low-risk execution  
The system can autonomously perform pre-approved low-risk actions, such as tagging, linking, summarizing, or updating internal memory.

Level 4: Fully autonomous scoped workflows  
The system can execute a bounded workflow end-to-end under user-defined policies.

The harness should default to conservative autonomy and escalate to the user when uncertain.

### 6. Idempotency and Deduplication

The harness must avoid duplicate updates.

Before creating or updating anything, it should check:

- Does this Linear issue already exist?
- Has this Attio note already been added?
- Is this open item already tracked?
- Is this GitHub activity already linked?
- Is this knowledge-base memory already present?
- Is this email thread already summarized?

Every external write should be idempotent where possible.

### 7. Provenance and Auditability

Every summary, memory update, and external action should include:

- Source
- Timestamp
- Tool used
- Agent responsible
- Confidence level
- Linked entities
- Previous state, where applicable
- New state, where applicable

The user should be able to inspect why the system made a recommendation.

### 8. Human-in-the-Loop Controls

The system must ask for approval when:

- Sending emails
- Creating or materially changing Linear issues
- Updating Attio records
- Making GitHub changes
- Deleting, archiving, or moving information
- Changing priorities or ownership
- Acting on ambiguous context
- Making high-impact customer, investor, or product updates

The user should be able to approve, reject, edit, or delegate follow-up.

### 9. Tooling Architecture

Use the MCP gateway for all tool calls.

Use the LLM gateway for model selection, routing, fallback, cost control, and observability.

Use the TUI as the primary human control interface.

Use the knowledge base as the durable memory layer.

The harness should not tightly couple business logic to any one tool. Tool adapters should be replaceable.

### 10. Observability

The harness should expose:

- Agent traces
- Tool calls
- LLM calls
- Cost and token usage
- Failed actions
- Pending approvals
- Memory updates
- Event processing history
- Subagent decisions
- Confidence levels

The user should be able to debug why an agent acted or failed to act.

## Required Output

Produce a complete harness design and implementation plan.

Your output should include:

1. Proposed harness architecture
2. Core orchestrator design
3. Subagent design
4. Memory model
5. Context graph model
6. Event model
7. Tool adapter model
8. TUI surfaces
9. Approval and autonomy model
10. Data schemas for key objects
11. Agent prompts or system instructions for each subagent
12. Workflow examples
13. Implementation phases
14. Risks and mitigations
15. Definition of done

## Key Workflow Examples to Support

The harness should support at least the following workflows.

### Workflow 1: Daily Operating Brief

Every morning or on demand, generate:

- Important emails
- People needing attention
- Customer updates
- Team updates
- Open items
- Product planning changes
- Linear/GitHub activity
- Pending approvals
- Recommended focus areas

### Workflow 2: Important Person Interaction

When an important person emails or appears in a meeting:

- Retrieve prior context
- Summarize relationship history
- Identify open commitments
- Identify relevant customer/product/project context
- Suggest response or next action
- Update the people focus list

### Workflow 3: Customer Discussion to Product Planning

When a customer discussion happens:

- Summarize the discussion
- Extract pain points, feature asks, objections, and next steps
- Update Attio
- Create or suggest Linear issues
- Link to roadmap items
- Store durable memory in GBrain
- Surface repeated asks across customers

### Workflow 4: Team Discussion to Execution

When an internal discussion happens:

- Summarize the discussion
- Extract decisions, action items, blockers, and open questions
- Update product planning context
- Suggest Linear updates
- Link related GitHub work
- Store decision records in GBrain

### Workflow 5: GitHub to Product Context

When GitHub activity happens:

- Summarize what changed
- Link implementation work to Linear and roadmap items
- Detect product implications
- Suggest release notes or internal update language
- Update GBrain with relevant durable context

### Workflow 6: Open Loop Closure

When an item appears resolved:

- Detect evidence of completion
- Verify against source systems
- Propose closure
- Update the relevant system after approval
- Archive or mark complete in the open-items queue

## Design Principles

The harness should be:

- Agent-native
- Persistent
- Event-driven
- Tool-agnostic
- Memory-first
- Human-in-the-loop by default
- Idempotent
- Auditable
- Secure
- Modular
- Useful through the TUI
- Capable of progressively higher autonomy

The final result should feel like the user has an operating system where agent processes continuously maintain context, organize work, surface what matters, and execute approved workflows across all connected tools.
