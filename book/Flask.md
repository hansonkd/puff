# Step 1: Setup your Puff project

Follow the instructions on how to setup a new poetry project and install Puff:

* [Using the CLI](https://github.com/hansonkd/puff/blob/master/book/CLI.md)

# Step 2: Add a Flask app

Install Flask to your your project:

```
poetry add flask
```

Add your flask application in a new file `my_puff_project/flask.py`

```python
from flask import Flask

app = Flask(__name__)

@app.route("/")
def hello_world():
    return "<p>Hello, World!</p>"
```

update `puff.toml`

```toml
wsgi = "my_puff_project.flask.app"
```

Run the server:

```bash
poetry run puff runserver
```