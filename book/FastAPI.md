# Step 1: Setup your Puff project

Follow the instructions on how to setup a new poetry project and install Puff:

* [Using the CLI](https://github.com/hansonkd/puff/blob/master/book/CLI.md)

# Step 2: Add a FastAPI app

Install FastAPI to your your project:

```
poetry add fastapi
```

Add your flask application in a new file `my_puff_project/fastapi.py`


```python
from fastapi import FastAPI


app = FastAPI()


@app.get("/fast-api")
async def read_root():
    return "<p>Hello, World!</p>"
```

update `puff.toml`

```toml
asgi = "my_puff_project.fastapi.app"
```

Run the server:

```bash
poetry run puff runserver
```