.PHONY: build run mcp serve watch install clean reset-db

build:
	cargo build

run:
	cargo run

mcp:
	cargo run -- mcp

serve:
	cargo run -- serve

watch:
	cargo watch -x run

install:
	cargo install --path .

clean:
	cargo clean

reset-db:
	@echo "WARNING: This will permanently delete ALL tasks in ~/.local/share/cli-todo/cli-todo.db"
	@echo ""
	@read -p "Type 'DELETE ALL TASKS' to confirm: " confirm && [ "$$confirm" = "DELETE ALL TASKS" ] || (echo "Aborted."; exit 1)
	rm -f ~/.local/share/cli-todo/cli-todo.db
	@echo "Database deleted."
