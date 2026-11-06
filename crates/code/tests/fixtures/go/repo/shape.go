// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0
package shape

const Scale = 2

type Shape interface {
	Area() int
}

type Square struct {
	Side int
}

// Area returns the Square area.
func (s Square) Area() int {
	return s.Side * s.Side * Scale
}

func NewSquare(side int) Square {
	return Square{Side: side}
}
