// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0
import { Counter, reset } from "./counter.js";
// Counter demo
export function run() {
  const note = "Counter";
  const c = new Counter();
  c.increment();
  reset(c);
  return c;
}

export const twice = (c) => c.increment() + c.increment();
