package main

import (
	"io"
	"log"
	"net"
)

func main() {
	ln, err := net.Listen("tcp", "127.0.0.1:8002")
	if err != nil {
		log.Fatal(err)
	}
	log.Println("go proxy listening on :8002 -> :9000")
	for {
		conn, err := ln.Accept()
		if err != nil {
			log.Println("accept:", err)
			continue
		}
		go proxy(conn)
	}
}

func proxy(client net.Conn) {
	defer client.Close()
	upstream, err := net.Dial("tcp", "127.0.0.1:9000")
	if err != nil {
		log.Println("dial upstream:", err)
		return
	}
	defer upstream.Close()
	done := make(chan struct{})
	go func() {
		io.Copy(upstream, client)
		done <- struct{}{}
	}()
	io.Copy(client, upstream)
	<-done
}
