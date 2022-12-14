# Puff CLI

You don't need to know any Rust to use Puff.

[Install Rust for your platform](https://www.rust-lang.org/tools/install) and then compile and install Puff:

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
greenlets = true
asyncio = false
dotenv = false
add_cwd_to_path = true
compression_middleware = true
dotenv_path = '.env'
pytest_path = './'
wsgi = 'my_wsgi.app'
asgi = 'my_asgi.app'

[cors]
base_config = 'Default'
max_age_secs = 86400
allow_credentials = true
allowed_origins = ['http://localhost:7777']
allowed_headers = '*'
allowed_methods = ['GET']

[[commands]]
function = 'my_python_mod.some_func'
command_name = 'execute_func'

[[postgres]]
name = 'default'

[[redis]]
name = 'default'

[[pubsub]]
name = 'default'

[[task_queue]]
name = 'default'
pool_size = 10
max_concurrent_tasks = 100

[[http_client]]
name = 'default'
http2_prior_knowledge = true
max_idle_connections = 100
user_agent = 'puff/0.1.0'

[[graphql]]
name = 'default'
schema = 'my_python_mod.Schema'
url = '/graphql/'
database = 'default'
subscription_url = '/subscriptions/'
playground_url = '/playground/'
```

# Step 1: Configure Python Project

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

# Step 2: Add Support Databases

Puff is built to use Postgres and Redis as support services. They are optional, but it's good to have them setup and ready to go.

The easiest way to get your databases setup is to use docker. Here is an example `docker-compose.yaml` that will work with the default Puff settings.

```yaml
version: '3.7'

services:
  postgres:
    image: postgres
    environment:
      - POSTGRES_USER=postgres
      - POSTGRES_PASSWORD=password
      - POSTGRES_DB=postgres
    ports:
      - "5432:5432"

  redis:
    image: redis
    ports:
      - "6379:6379"
```

Add to your project as `docker-compose.yaml` and run `docker-compose up` in a new tab.


# Step 3: Add a Web Service

* [Flask](https://github.com/hansonkd/puff/blob/master/book/Flask.md)
* [FastAPI](https://github.com/hansonkd/puff/blob/master/book/FastAPI.md)
* [Django](https://github.com/hansonkd/puff/blob/master/book/Django.md)
