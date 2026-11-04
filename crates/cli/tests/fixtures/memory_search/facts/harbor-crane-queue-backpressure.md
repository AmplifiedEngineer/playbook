---
name: harbor-crane-queue-backpressure
description: Use when crane jobs pile up and the dock queue stalls.
type: project
---

Backpressure starts when the pending list passes two hundred jobs. The scheduler then refuses new lifts until the list drains below fifty.
