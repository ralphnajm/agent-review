#!/usr/bin/env bash
# Creates a demo repository for screenshots: a small Python API with a
# coding agent's uncommitted work on top.
#
#   scripts/demo-repo.sh [directory]    (default: target/demo/shelf)
set -euo pipefail

dir=${1:-target/demo/shelf}
rm -rf "$dir"
mkdir -p "$dir"
cd "$dir"
git init -q -b main
git config user.name "Demo"
git config user.email "demo@example.com"

mkdir -p app/routes tests

cat > pyproject.toml <<'EOF'
[project]
name = "shelf"
version = "0.3.0"
description = "A tiny API for keeping track of the books you own"
requires-python = ">=3.11"
dependencies = ["fastapi>=0.115", "sqlalchemy>=2.0", "uvicorn>=0.30"]

[project.optional-dependencies]
dev = ["pytest>=8", "httpx>=0.27"]
EOF

cat > README.md <<'EOF'
# Shelf

A tiny API for keeping track of the books you own.

## Running

    uv run uvicorn app.main:app --reload

## Endpoints

- `GET /books`: every book, sorted by title
- `POST /books`: add a book
- `GET /books/{id}`: one book
EOF

touch app/__init__.py app/routes/__init__.py

cat > app/main.py <<'EOF'
from fastapi import FastAPI

from app.routes import books

app = FastAPI(title="Shelf")
app.include_router(books.router)
EOF

cat > app/db.py <<'EOF'
from collections.abc import Iterator

from sqlalchemy import create_engine
from sqlalchemy.orm import Session, sessionmaker

engine = create_engine("sqlite:///shelf.db", connect_args={"check_same_thread": False})
SessionLocal = sessionmaker(bind=engine, autoflush=False)


def get_session() -> Iterator[Session]:
    session = SessionLocal()
    try:
        yield session
    finally:
        session.close()
EOF

cat > app/models.py <<'EOF'
from datetime import date

from sqlalchemy import Date, String
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column


class Base(DeclarativeBase):
    pass


class Book(Base):
    __tablename__ = "books"

    id: Mapped[int] = mapped_column(primary_key=True)
    title: Mapped[str] = mapped_column(String(200))
    author: Mapped[str] = mapped_column(String(120))
    added_on: Mapped[date] = mapped_column(Date, default=date.today)
EOF

cat > app/routes/books.py <<'EOF'
from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel
from sqlalchemy import select
from sqlalchemy.orm import Session

from app.db import get_session
from app.models import Book

router = APIRouter(prefix="/books", tags=["books"])


class BookIn(BaseModel):
    title: str
    author: str


class BookOut(BookIn):
    id: int


@router.get("", response_model=list[BookOut])
def list_books(session: Session = Depends(get_session)):
    return session.scalars(select(Book).order_by(Book.title)).all()


@router.post("", response_model=BookOut, status_code=201)
def add_book(book: BookIn, session: Session = Depends(get_session)):
    record = Book(title=book.title, author=book.author)
    session.add(record)
    session.commit()
    return record


@router.get("/{book_id}", response_model=BookOut)
def get_book(book_id: int, session: Session = Depends(get_session)):
    book = session.get(Book, book_id)
    if book is None:
        raise HTTPException(status_code=404, detail="Book not found")
    return book
EOF

cat > tests/test_books.py <<'EOF'
from fastapi.testclient import TestClient

from app.main import app

client = TestClient(app)


def test_add_and_fetch_book():
    created = client.post("/books", json={"title": "Dune", "author": "Frank Herbert"}).json()
    fetched = client.get(f"/books/{created['id']}").json()
    assert fetched == created


def test_missing_book_is_404():
    assert client.get("/books/9999").status_code == 404
EOF

git add -A
git commit -q -m "Books API with create, list and fetch"

# ----- The agent's uncommitted work: pagination and search for GET /books -----

cat > README.md <<'EOF'
# Shelf

A tiny API for keeping track of the books you own.

## Running

    uv run uvicorn app.main:app --reload

Set `SHELF_DATABASE_URL` to use another database than `shelf.db`.

## Endpoints

- `GET /books`: books sorted by title, 20 at a time. Takes `q` to search
  titles and authors, and `limit` and `offset` to page through the results.
- `POST /books`: add a book
- `GET /books/{id}`: one book
EOF

cat > app/db.py <<'EOF'
import os
from collections.abc import Iterator

from sqlalchemy import create_engine
from sqlalchemy.orm import Session, sessionmaker

DATABASE_URL = os.environ.get("SHELF_DATABASE_URL", "sqlite:///shelf.db")

engine = create_engine(DATABASE_URL, connect_args={"check_same_thread": False})
SessionLocal = sessionmaker(bind=engine, autoflush=False)


def get_session() -> Iterator[Session]:
    session = SessionLocal()
    try:
        yield session
    finally:
        session.close()
EOF

cat > app/models.py <<'EOF'
from datetime import date

from sqlalchemy import Date, String
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column


class Base(DeclarativeBase):
    pass


class Book(Base):
    __tablename__ = "books"

    id: Mapped[int] = mapped_column(primary_key=True)
    title: Mapped[str] = mapped_column(String(200), index=True)
    author: Mapped[str] = mapped_column(String(120), index=True)
    added_on: Mapped[date] = mapped_column(Date, default=date.today)
EOF

cat > app/pagination.py <<'EOF'
"""Offset pagination for list endpoints."""

from typing import Generic, TypeVar

from pydantic import BaseModel
from sqlalchemy import Select, func, select
from sqlalchemy.orm import Session

T = TypeVar("T")


class Page(BaseModel, Generic[T]):
    items: list[T]
    total: int
    limit: int
    offset: int


def paginate(session: Session, query: Select, *, limit: int, offset: int) -> dict:
    """Runs `query` for one page of results, with the total count."""
    total = session.scalar(select(func.count()).select_from(query.subquery()))
    items = session.scalars(query.limit(limit).offset(offset)).all()
    return {"items": items, "total": total, "limit": limit, "offset": offset}
EOF

cat > app/routes/books.py <<'EOF'
from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel
from sqlalchemy import select
from sqlalchemy.orm import Session

from app.db import get_session
from app.models import Book
from app.pagination import Page, paginate

router = APIRouter(prefix="/books", tags=["books"])


class BookIn(BaseModel):
    title: str
    author: str


class BookOut(BookIn):
    id: int


@router.get("", response_model=Page[BookOut])
def list_books(
    q: str | None = None,
    limit: int = 20,
    offset: int = 0,
    session: Session = Depends(get_session),
):
    query = select(Book).order_by(Book.title)
    if q:
        pattern = f"%{q}%"
        query = query.where(Book.title.ilike(pattern) | Book.author.ilike(pattern))
    return paginate(session, query, limit=limit, offset=offset)


@router.post("", response_model=BookOut, status_code=201)
def add_book(book: BookIn, session: Session = Depends(get_session)):
    record = Book(title=book.title, author=book.author)
    session.add(record)
    session.commit()
    return record


@router.get("/{book_id}", response_model=BookOut)
def get_book(book_id: int, session: Session = Depends(get_session)):
    book = session.get(Book, book_id)
    if book is None:
        raise HTTPException(status_code=404, detail="Book not found")
    return book
EOF

cat > tests/conftest.py <<'EOF'
import pytest
from fastapi.testclient import TestClient

from app.main import app
from app.models import Base
from app.db import engine


@pytest.fixture
def client():
    Base.metadata.create_all(engine)
    yield TestClient(app)
    Base.metadata.drop_all(engine)
EOF

cat > tests/test_books.py <<'EOF'
def add(client, title, author):
    return client.post("/books", json={"title": title, "author": author}).json()


def test_add_and_fetch_book(client):
    created = add(client, "Dune", "Frank Herbert")
    fetched = client.get(f"/books/{created['id']}").json()
    assert fetched == created


def test_missing_book_is_404(client):
    assert client.get("/books/9999").status_code == 404


def test_list_is_paginated(client):
    for n in range(25):
        add(client, f"Book {n:02}", "Anonymous")
    page = client.get("/books", params={"limit": 10, "offset": 20}).json()
    assert page["total"] == 25
    assert [b["title"] for b in page["items"]] == [f"Book {n}" for n in range(20, 25)]


def test_search_matches_title_and_author(client):
    add(client, "Dune", "Frank Herbert")
    add(client, "Emma", "Jane Austen")
    add(client, "Persuasion", "Jane Austen")
    titles = [b["title"] for b in client.get("/books", params={"q": "austen"}).json()["items"]]
    assert titles == ["Emma", "Persuasion"]
EOF

echo "Demo repository ready in $(pwd)"
