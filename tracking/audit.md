# Orchestrator Audit Log

Every routing, dispatch, and merge decision logs here with
reasoning. The human reviews daily. Newest entries at the bottom.

Entry format:

```
[YYYY-MM-DD HH:MM UTC] <action>
  Input: <what triggered this>
  Options: <what was considered>
  Decision: <what was chosen>
  Reasoning: <why>
  Follow-up: <next actions queued>
```

Daily digest format (appended at day boundary or block
completion):

```
═══ DAILY DIGEST — YYYY-MM-DD ═══
Dispatched: <sessions launched today>
Merged: <PRs merged today>
Queued: <work moved into ready state>
Blocked: <work still blocked, on what>
Escalations raised: <count + pointers to escalations.md>
Notes: <anything the human should know>
```

---

(no entries yet — first entry lands when Block 1 begins)
