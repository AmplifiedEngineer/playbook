package shape
// Square totals.
func Total(items []Square) int {
	label := "Square"
	sum := 0
	for _, s := range items {
		sum += s.Area()
	}
	return sum + len(label)
}

func Demo() int {
	return Total([]Square{NewSquare(2), NewSquare(3)})
}
