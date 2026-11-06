import { Square } from "./shapes";
// Square factory
export function makeSquare(side: number): Square {
  const label = "Square";
  return new Square(side);
}

export function total(items: Square[]): number {
  return items.reduce((sum, s) => sum + s.area(), 0);
}
