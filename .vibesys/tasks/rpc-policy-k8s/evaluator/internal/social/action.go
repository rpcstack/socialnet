package social

import (
	"strconv"

	"github.com/uw-syfi/vibesys/sdk/vibesys-evaluator/go/workload"
)

// Kinds of actions: what one operation of the accuracy workload does.
const (
	KindCompose  = "compose"
	KindFollow   = "follow"
	KindUnfollow = "unfollow"
	KindRead     = "read"
)

// Invalid requests the accuracy workload sends on purpose, and what the application answers.
const (
	InvalidNoText      = "no-text"        // compose without text: 400 Incomplete arguments
	InvalidNoFollowee  = "no-followee"    // follow without followee_id: 400 Incomplete arguments
	InvalidUser        = "bad-user"       // read with user_id "abc": 400 not a number
	InvalidEmptyWindow = "empty-window"   // read of [5, 5): 200 []
	InvalidNegative    = "negative-start" // read of [-1, 10): 200 []
)

// Action is what one operation of the accuracy workload sends, recorded as its Info: a stimulus,
// never an expected answer. The oracle reads it back to judge the answer.
type Action struct {
	Kind string `json:"kind"`
	// User is the author (compose), the follower (follow, unfollow) or the reader (read).
	User int64 `json:"user"`
	// Target is the followee (follow, unfollow).
	Target int64 `json:"target,omitempty"`
	// Compose: the text as sent, its unique marker ("m...", the text has "#" and it), the user it
	// mentions ("@user<id>" in the text; 0 for none), the URL ending it ("" for none), the media.
	Text    string  `json:"text,omitempty"`
	Marker  string  `json:"marker,omitempty"`
	Mention int64   `json:"mention,omitempty"`
	URL     string  `json:"url,omitempty"`
	Media   []Media `json:"media,omitempty"`
	// Read: the home (or user) timeline, entries [Start, Stop).
	Home  bool `json:"home,omitempty"`
	Start int  `json:"start,omitempty"`
	Stop  int  `json:"stop,omitempty"`
	// Invalid names the malformed request sent instead (Invalid* constants), "" for none.
	Invalid string `json:"invalid,omitempty"`
}

// Step is the action's HTTP request.
func (a Action) Step() workload.Step {
	id := strconv.FormatInt(a.User, 10)
	switch a.Kind {
	case KindCompose:
		if a.Invalid == InvalidNoText {
			return workload.Post(RouteCompose, workload.Form("user_id", id), workload.Form("username", Username(a.User)),
				workload.Form("post_type", "0"))
		}
		return Compose(a.User, a.Text, a.Media)
	case KindFollow, KindUnfollow:
		if a.Invalid == InvalidNoFollowee {
			route := RouteFollow
			if a.Kind == KindUnfollow {
				route = RouteUnfollow
			}
			return workload.Post(route, workload.Form("user_id", id))
		}
		return Follow(a.User, a.Target, a.Kind == KindFollow)
	default:
		switch a.Invalid {
		case InvalidUser:
			return Read(a.Home, "abc", "0", strconv.Itoa(PageSize))
		case InvalidEmptyWindow:
			return Read(a.Home, id, "5", "5")
		case InvalidNegative:
			return Read(a.Home, id, "-1", strconv.Itoa(PageSize))
		}
		return ReadPage(a.Home, a.User, a.Start, a.Stop)
	}
}

// Composed is what the oracle needs of a compose to judge the post it made.
func (a Action) Composed() *Composed {
	return &Composed{Author: a.User, Text: a.Text, Mention: a.Mention, URL: a.URL, Media: a.Media}
}
