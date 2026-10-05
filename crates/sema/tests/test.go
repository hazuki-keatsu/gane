// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

// MVP semantic-analysis smoke test: no imports and no Go runtime required.
package main

const Limit int = 4

var initial int = 1

type Point struct {
	x int
	y int
}

// Pointer recursion is valid because the recursive edge is indirect.
type Node struct {
	value int
	next  *Node
}

func add(left int, right int) int {
	return left + right
}

func main() {
	var point Point
	point.x = initial
	point.y = add(point.x, Limit)

	var pointPtr = &point
	pointPtr.x = pointPtr.x + 1

	var values [4]int
	values[0] = pointPtr.x
	var index int = 1
	for index < Limit {
		values[index] = values[index - 1] + index
		index = index + 1
	}

	var node Node
	node.value = values[3]
	node.next = &node
	if node.next.value > 0 {
		node.value = node.value + 1
	} else {
		node.value = 0
	}

	var done bool = node.value > 0
	if !done {
		return
	}
}
