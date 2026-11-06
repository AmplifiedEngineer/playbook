// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0
export const SCALE = 2;

export interface Shape {
  name: string;
  area(): number;
}

export enum Axis {
  Horizontal,
  Vertical,
}

export class Square implements Shape {
  name = "Square";
  constructor(private side: number) {}
  // Square area
  area(): number {
    return this.side * this.side * SCALE;
  }
}

export type Id = string;
