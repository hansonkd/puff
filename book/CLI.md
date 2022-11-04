# Puff CLI

You don't need to know any Rust to use Puff.

Install Rust for your platform and then compile and install Puff for your platform.

```bash
cargo install puff-rs
```

Check to see if puff was installed correctly:

```bash
puff help
```

To get started with Puff, you need a `puff.toml`. Show an example of this file by use `puff example_config`:

```toml
django = false
postgres = false
redis = false
pubsub = false
task_queue = false
greenlets = true
asyncio = false
dotenv = false
dotenv_path = '.env'
pytest_path = './'
wsgi = 'my_wsgi.app'
asgi = 'my_asgi.app'
graphql = 'my_graphql.Schema'
graphql_url = '/graphql/'
graphql_subscription_url = '/subscriptions/'

[[commands]]
function = 'my_python_mod.some_func'
command_name = 'execute_func'
```

# Step 1: Hello World

Start a new python project with poetry:

```bash
poetry new my_puff_project
cd my_puff_project
```

Edit `my_puff_project/__init__.py` to add a new function `hello_world`:

```python
def hello_world():
    print("hi from puff")
```

and create `puff.toml` with a minimal config defining your function.

```toml
[[commands]]
function = 'my_puff_project.hello_world'
command_name = 'hello_world'
```

Now run your function:

```
poetry run puff hello_world
```

# Step 2: Add an WSGI app

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
