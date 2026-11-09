# SPDX-License-Identifier: Apache-2.0


class Cart:
    """Items a customer wants to buy."""

    def __init__(self):
        self.items = []

    def add(self, name, cents):
        """Adds one item to the cart."""
        self.items.append((name, cents))

    def total_cents(self):
        """Returns the cart total in cents."""
        return sum(cents for _, cents in self.items)


def apply_discount(total_cents, percent):
    """Returns the total after a percentage discount."""
    return total_cents - total_cents * percent // 100
