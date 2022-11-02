from fastapi import FastAPI
from puff import global_state, wrap_async


app = FastAPI()


@app.get("/fast-api")
async def read_root():
    return {"Hello": "World", "from": "Fast API"}


@app.get("/fast-api-async")
async def read_root():
    result = await wrap_async(
        lambda r: global_state.hello_from_rust_async(r, "hello from asyncio")
    )
    return {"Hello": "World", "from": "Fast API", "rust_value": result}
