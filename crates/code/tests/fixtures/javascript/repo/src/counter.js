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
