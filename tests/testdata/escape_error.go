package main

func leak() *int {
	var local int
	return &local
}

func main() {
	var r *int = leak()
}
