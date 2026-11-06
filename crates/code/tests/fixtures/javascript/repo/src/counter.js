// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0
export const START = 0;

export class Counter {
  constructor() {
    this.value = START;
  }
  // Counter increments the value
  increment() {
    this.value += 1;
    return this.value;
  }
}

export function reset(counter) {
  counter.value = START;
}
