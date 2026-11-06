package social

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"time"
)

// SeedWorkers is how many requests the seed keeps in flight.
const SeedWorkers = 32

// Populate registers the dataset's users and then its follow edges through the frontend at base
// (the application starts empty and seeds nothing of its own except the user-mention table).
// A request that fails is retried a few times: the seed runs as soon as the services serve.
func (d *Dataset) Populate(ctx context.Context, base string) error {
	cl := &http.Client{Timeout: 30 * time.Second}
	post := func(route string, form url.Values, want string) error {
		var last error
		for try := 0; try < 6; try++ {
			if try > 0 {
				select {
				case <-time.After(time.Duration(try) * 500 * time.Millisecond):
				case <-ctx.Done():
					return ctx.Err()
				}
			}
			req, err := http.NewRequestWithContext(ctx, "POST", base+route, strings.NewReader(form.Encode()))
			if err != nil {
				return err
			}
			req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
			resp, err := cl.Do(req)
			if err != nil {
				last = err
				continue
			}
			body, _ := io.ReadAll(resp.Body)
			resp.Body.Close()
			if resp.StatusCode == 200 && string(body) == want {
				return nil
			}
			last = fmt.Errorf("%s %v: status %d, %q", route, form, resp.StatusCode, body)
		}
		return last
	}
	run := func(n int, one func(i int) error) error {
		ctx, cancel := context.WithCancel(ctx)
		defer cancel()
		var wg sync.WaitGroup
		var once sync.Once
		var first error
		next := make(chan int)
		for w := 0; w < SeedWorkers; w++ {
			wg.Add(1)
			go func() {
				defer wg.Done()
				for i := range next {
					if err := one(i); err != nil {
						once.Do(func() { first = err; cancel() })
					}
				}
			}()
		}
	feed:
		for i := 0; i < n; i++ {
			select {
			case next <- i:
			case <-ctx.Done():
				break feed
			}
		}
		close(next)
		wg.Wait()
		return first
	}
	id := func(v int64) string { return strconv.FormatInt(v, 10) }
	if err := run(d.Users, func(i int) error {
		u := int64(i + 1)
		return post(RouteRegister, url.Values{
			"first_name": {"first" + id(u)}, "last_name": {"last" + id(u)},
			"username": {Username(u)}, "password": {Password(u)}, "user_id": {id(u)},
		}, MsgSuccess)
	}); err != nil {
		return fmt.Errorf("registering users: %w", err)
	}
	if err := run(d.Users, func(i int) error {
		u := int64(i + 1)
		for _, v := range d.Followees(u) {
			if err := post(RouteFollow, url.Values{"user_id": {id(u)}, "followee_id": {id(int64(v))}}, MsgSuccess); err != nil {
				return err
			}
		}
		return nil
	}); err != nil {
		return fmt.Errorf("following: %w", err)
	}
	return nil
}
