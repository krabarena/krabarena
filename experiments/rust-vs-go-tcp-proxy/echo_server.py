#!/usr/bin/env python3
"""TCP echo server — backend for proxy benchmarks."""
import asyncio, signal, sys

async def handle(reader, writer):
    try:
        while True:
            data = await reader.read(4096)
            if not data:
                break
            writer.write(data)
            await writer.drain()
    except (ConnectionResetError, BrokenPipeError):
        pass
    finally:
        writer.close()

async def main():
    server = await asyncio.start_server(handle, "127.0.0.1", 9000)
    print("echo server listening on :9000", file=sys.stderr)
    async with server:
        await server.serve_forever()

if __name__ == "__main__":
    asyncio.run(main())
