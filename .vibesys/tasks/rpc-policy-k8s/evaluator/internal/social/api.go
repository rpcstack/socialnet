package social

import (
	"bytes"
	"encoding/json"
	"fmt"
	"regexp"
	"sort"
	"strconv"
	"strings"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/workload"
)

// The frontend's HTTP routes (crates/socialnet/src/frontend/http.rs), the nginx/lua gateway's of
// DeathStarBench: write routes take a urlencoded form, read routes a query string.
const (
	RouteCompose      = "/wrk2-api/post/compose"
	RouteRegister     = "/wrk2-api/user/register"
	RouteFollow       = "/wrk2-api/user/follow"
	RouteUnfollow     = "/wrk2-api/user/unfollow"
	RouteUserTimeline = "/wrk2-api/user-timeline/read"
	RouteHomeTimeline = "/wrk2-api/home-timeline/read"
)

// The answers of the write routes: 200 with the text and a newline (lua's ngx.say).
const (
	MsgComposed = "Successfully upload post\n"
	MsgSuccess  = "Success!\n"
	// MsgIncomplete is the 400 of a request without a required argument.
	MsgIncomplete = "Incomplete arguments\n"
)

// ShortURLPrefix is what the url-shorten service puts before a random 10-character suffix.
const ShortURLPrefix = "http://short-url/"

// PageSize is the posts of one timeline page the workloads read.
const PageSize = 10

// Compose is the request to post text as user id, with optional media (ids and types).
func Compose(id int64, text string, media []Media) workload.Step {
	opts := []workload.Option{
		workload.Form("user_id", strconv.FormatInt(id, 10)), workload.Form("username", Username(id)),
		workload.Form("post_type", "0"), workload.Form("text", text),
	}
	if len(media) > 0 {
		ids, types := make([]string, len(media)), make([]string, len(media))
		for i, m := range media {
			ids[i], types[i] = m.ID, m.Type
		}
		idJSON, _ := json.Marshal(ids)
		typeJSON, _ := json.Marshal(types)
		opts = append(opts, workload.Form("media_ids", string(idJSON)), workload.Form("media_types", string(typeJSON)))
	}
	return workload.Post(RouteCompose, opts...)
}

// Register is the registration of user id.
func Register(id int64) workload.Step {
	return workload.Post(RouteRegister,
		workload.Form("first_name", "first"+strconv.FormatInt(id, 10)), workload.Form("last_name", "last"+strconv.FormatInt(id, 10)),
		workload.Form("username", Username(id)), workload.Form("password", Password(id)),
		workload.Form("user_id", strconv.FormatInt(id, 10)))
}

// Follow is a follow (on) or an unfollow of u -> v, by id.
func Follow(u, v int64, on bool) workload.Step {
	route := RouteFollow
	if !on {
		route = RouteUnfollow
	}
	return workload.Post(route, workload.Form("user_id", strconv.FormatInt(u, 10)), workload.Form("followee_id", strconv.FormatInt(v, 10)))
}

// Read is a read of user's home (or user) timeline, entries [start, stop), with the values as
// sent (any text, so that invalid requests can be sent too).
func Read(home bool, user, start, stop string) workload.Step {
	route := RouteUserTimeline
	if home {
		route = RouteHomeTimeline
	}
	return workload.Get(route, workload.Query("user_id", user), workload.Query("start", start), workload.Query("stop", stop))
}

// ReadPage is Read of a numeric user and window.
func ReadPage(home bool, user int64, start, stop int) workload.Step {
	return Read(home, strconv.FormatInt(user, 10), strconv.Itoa(start), strconv.Itoa(stop))
}

// ------------------------------------------------------------------ served posts

// Post is one post as the frontend serves it in a timeline (http.rs PostJson).
type Post struct {
	PostID    string    `json:"post_id"`
	Creator   Creator   `json:"creator"`
	ReqID     string    `json:"req_id"`
	Text      string    `json:"text"`
	Mentions  []Mention `json:"user_mentions"`
	Media     []Media   `json:"media"`
	URLs      []URL     `json:"urls"`
	Timestamp string    `json:"timestamp"`
	PostType  int       `json:"post_type"`

	// TS is Timestamp parsed (milliseconds since the epoch).
	TS int64 `json:"-"`
}

type Creator struct {
	UserID   string `json:"user_id"`
	Username string `json:"username"`
}

type Mention struct {
	UserID   string `json:"user_id"`
	Username string `json:"username"`
}

type Media struct {
	ID   string `json:"media_id"`
	Type string `json:"media_type"`
}

type URL struct {
	Shortened string `json:"shortened_url"`
	Expanded  string `json:"expanded_url"`
}

var decimal = regexp.MustCompile(`^-?[0-9]+$`)

// ParseTimeline decodes a timeline answer strictly: a JSON array of posts with exactly the
// frontend's fields (an empty list is []), ids and the timestamp decimal strings.
func ParseTimeline(body []byte) ([]Post, error) {
	dec := json.NewDecoder(bytes.NewReader(body))
	dec.DisallowUnknownFields()
	var posts []Post
	if err := dec.Decode(&posts); err != nil {
		return nil, fmt.Errorf("not a JSON array of posts: %v", err)
	}
	if posts == nil {
		return nil, fmt.Errorf("null, want a JSON array")
	}
	if dec.More() {
		return nil, fmt.Errorf("trailing data after the JSON array")
	}
	for i := range posts {
		p := &posts[i]
		for _, f := range []struct{ name, v string }{{"post_id", p.PostID}, {"req_id", p.ReqID}, {"timestamp", p.Timestamp}, {"creator.user_id", p.Creator.UserID}} {
			if !decimal.MatchString(f.v) {
				return nil, fmt.Errorf("post %d: %s %q is not a decimal integer", i, f.name, f.v)
			}
		}
		if p.Mentions == nil || p.Media == nil || p.URLs == nil {
			return nil, fmt.Errorf("post %d: user_mentions, media and urls must be arrays", i)
		}
		p.TS, _ = strconv.ParseInt(p.Timestamp, 10, 64)
	}
	return posts, nil
}

// MarkerOf is the marker token ("m...", without its '#') of a post's text, or "".
func MarkerOf(text string) string {
	at := strings.Index(text, "#m")
	if at < 0 {
		return ""
	}
	end := at + 1
	for end < len(text) && text[end] != ' ' {
		end++
	}
	return text[at+1 : end]
}

// ------------------------------------------------------------------ what a compose stores

// Composed is what a compose request sent, as the oracle needs it to judge the post it made.
type Composed struct {
	Author  int64
	Text    string // as sent
	Mention int64  // the user "@user<id>" in the text mentions, 0 for none
	URL     string // the URL ending the text, "" for none
	Media   []Media
}

var shortURL = regexp.MustCompile(`^` + regexp.QuoteMeta(ShortURLPrefix) + `[a-zA-Z0-9]{10}$`)

// Check says what is wrong with p as the post this compose made, or "" if nothing is. The
// application stores the text with the mention replaced by "user_id:<id>" (the text service) and
// the URL by a shortened one ("http://short-url/" and 10 random characters; the compose-post
// service shortens it again on its own for the post's urls, so the two may differ); the mention
// is the user-mention service's fake user of that id, its username suffixed "@mongodb" or
// "@memcached" (whichever answered).
func (c *Composed) Check(p *Post) string {
	if p.Creator.UserID != strconv.FormatInt(c.Author, 10) || p.Creator.Username != Username(c.Author) {
		return fmt.Sprintf("creator (%s, %s), want (%d, %s)", p.Creator.UserID, p.Creator.Username, c.Author, Username(c.Author))
	}
	if p.PostType != 0 {
		return fmt.Sprintf("post_type %d, want 0", p.PostType)
	}
	text := c.Text
	if c.Mention != 0 {
		text = strings.Replace(text, "@"+Username(c.Mention), "user_id:"+strconv.FormatInt(c.Mention, 10), 1)
		id := strconv.FormatInt(c.Mention, 10)
		m := p.Mentions
		if len(m) != 1 || m[0].UserID != id || (m[0].Username != Username(c.Mention)+"@mongodb" && m[0].Username != Username(c.Mention)+"@memcached") {
			return fmt.Sprintf("user_mentions %v, want user %s (%s@mongodb or @memcached)", m, id, Username(c.Mention))
		}
	} else if len(p.Mentions) != 0 {
		return fmt.Sprintf("user_mentions %v, want none", p.Mentions)
	}
	if c.URL != "" {
		if len(p.URLs) != 1 || p.URLs[0].Expanded != c.URL || !shortURL.MatchString(p.URLs[0].Shortened) {
			return fmt.Sprintf("urls %v, want %s shortened", p.URLs, c.URL)
		}
		head := strings.TrimSuffix(text, c.URL)
		if !strings.HasPrefix(p.Text, head) || !shortURL.MatchString(p.Text[len(head):]) {
			return fmt.Sprintf("text %q, want %q with the URL shortened", p.Text, text)
		}
	} else {
		if len(p.URLs) != 0 {
			return fmt.Sprintf("urls %v, want none", p.URLs)
		}
		if p.Text != text {
			return fmt.Sprintf("text %q, want %q", p.Text, text)
		}
	}
	if len(p.Media) != len(c.Media) {
		return fmt.Sprintf("media %v, want %v", p.Media, c.Media)
	}
	for i := range c.Media {
		if p.Media[i] != c.Media[i] {
			return fmt.Sprintf("media %v, want %v", p.Media, c.Media)
		}
	}
	return ""
}

// SortedIDs is a sorted copy of ids.
func SortedIDs(ids []int64) []int64 {
	s := append([]int64(nil), ids...)
	sort.Slice(s, func(i, j int) bool { return s[i] < s[j] })
	return s
}
