---
name: engineering-standards-javascript
description: JavaScript and TypeScript rules for code and tests, including validation and Jest or Vitest mocks. Use when writing or reviewing JS or TS.
user-invocable: false
---

# Engineering Standards: JavaScript and TypeScript

Language-specific engineering standards for JavaScript and TypeScript, derived from the team's base standards. RFC 2119 keywords (MUST, SHOULD) carry their standard meanings.

**REQUIRED BACKGROUND:** engineering-standards owns the general rules (PR readiness, size, test types and requirements, the mocking philosophy, deployment). Read it first. This skill adds only the JS/TS specifics and never restates the base.

## Validation and parsing: use a schema validator

**Prefer a schema validator (Zod, Valibot, or whatever the project already uses) for validation and parsing** over hand-written type guards, manual `if` checks, or casting with `as`. A schema is one source of truth: it validates at runtime and gives you the static type (Zod's `z.infer`, Valibot's `v.InferOutput`), so the shape and the check can't drift.

**Validate at the boundaries of the application.** Parse untrusted or untyped data the moment it enters the app, then work with typed values inside. Boundaries include:

- HTTP request bodies, query params, route params, and headers.
- Environment variables and config files.
- Responses from external APIs and third-party services.
- Message-queue and event payloads.
- Anything typed `any` or `unknown`: `JSON.parse` output, untyped DB rows, files read off disk.

Use `schema.parse(input)` when a bad value should throw and fail fast at the edge, or `schema.safeParse(input)` when you want to handle the error path yourself. Never cast across a boundary (`input as User`): `as` is a compile-time assertion with no runtime check, so malformed data flows straight in.

### Pattern

Zod example (Valibot follows the same shape: define the schema, derive the type from it, parse at the edge):

```ts
import { z } from 'zod';

const CreateUser = z.object({
  email: z.string().email(),
  age: z.number().int().min(0),
});
type CreateUser = z.infer<typeof CreateUser>;  // type derived from the schema

app.post('/users', (req, res) => {
  const body = CreateUser.parse(req.body);  // throws at the boundary on bad input
  // body is typed CreateUser from here on
});
```

## Testing: mocking in Jest or Vitest

**Prefer `mockDeep` for mocking objects, services, and interfaces.** Use `mockDeep<T>()` from `jest-mock-extended` (Jest) or `vitest-mock-extended` (Vitest) instead of hand-written `jest.fn()` object literals or `jest.mock()` module mocks.

Why:

- Type-safe: the mock is typed as `T`, so a change to the real interface breaks the test at compile time instead of passing against a stale shape.
- Deep by default: nested properties and methods are auto-stubbed, so `client.user.findMany()` works with no extra wiring.
- Less boilerplate: no per-method `jest.fn()`. The whole surface is mocked, and every call is a spy you can assert on.

This is the base rule "spy on the interface and verify call parameters" made concrete: a `mockDeep<T>()` is the typed interface spy. Inject it (constructor or parameter) rather than `jest.mock()`-ing the module, per the base rule "prefer dependency injection over global mocking".

### Pattern

```ts
// Jest: 'jest-mock-extended'   Vitest: 'vitest-mock-extended'
import { mockDeep, mockReset } from 'jest-mock-extended';
import type { OrderService } from './order-service';

const orders = mockDeep<OrderService>();  // fully typed, deep-mocked
beforeEach(() => mockReset(orders));       // fresh per test, no bleed

test('charges the customer once', async () => {
  orders.payments.charge.mockResolvedValue({ id: 'ch_1', status: 'paid' });
  await placeOrder(orders, cart);          // inject the mock
  expect(orders.payments.charge).toHaveBeenCalledWith(cart.total);
});
```

### When not to reach for mockDeep

- A single standalone function: `jest.fn()` or `vi.fn()` is enough.
- A module side effect you can't inject (a logger imported for its effect): `jest.mock()` or `vi.mock()` at the boundary.
- Anything the base rule says to leave alone: tables or services owned by another team, or a dependency you can exercise with a real object instead of a mock.

## Common mistakes

- Repeating a bare string literal for a status, mode, or key. Define the values once in an `as const` object and derive the union type from it (`type Status = typeof Status[keyof typeof Status]`). Prefer that over a TS `enum`, which emits a runtime object and behaves differently under `const enum` and `isolatedModules`.
