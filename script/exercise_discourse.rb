# Drive discourse's own service and model layer, so the gold set records real
# dispatches in organically-written app code.
#
#     cd ~/code/lib/ruby/discourse
#     TREKR_EXERCISE=/path/to/trekr/script/exercise_discourse.rb \
#       bin/rails runner /path/to/trekr/script/trace_gold.rb
#
# widget_shop was written *for* this evaluation: 137 lines of shapes the
# receiver ladder has a rung for. Discourse was not written for anything of the
# kind — 1,247 app files, 224 service objects, no Sorbet — which is the point.
#
# Every call is wrapped: a service that needs a request, a site setting or the
# network fails on its own and the run continues. A partial trace is still a
# trace, and the alternative is one missing dependency costing the corpus.

require "securerandom"

Rails.application.eager_load!
require "fabrication"
Dir[Rails.root.join("spec/fabricators/**/*.rb")].sort.each { |f| load f }

def attempt(label)
  yield
rescue StandardError, NotImplementedError => e
  warn "  skipped #{label}: #{e.class}"
end

tag = SecureRandom.hex(4)
user =
  Fabricate(
    :user,
    username: "trekr#{tag}",
    email: "trekr#{tag}@example.com",
    name: "Trekr #{tag}",
  )
other =
  Fabricate(
    :user,
    username: "other#{tag}",
    email: "other#{tag}@example.com",
    name: "Other #{tag}",
  )
category = Fabricate(:category, user: user, name: "Cat #{tag}", slug: "cat-#{tag}")
topic = Fabricate(:topic, user: user, category: category, title: "A topic about #{tag} things")
post = Fabricate(:post, topic: topic, user: user, raw: "Some body text for #{tag}, long enough.")

# Guardian — a PORO holding a user, asked hundreds of permission questions.
# The densest ordinary-Ruby surface in the app.
guardian = Guardian.new(user)
attempt("guardian") do
  guardian.can_see?(topic)
  guardian.can_edit?(post)
  guardian.can_create_post?(topic)
  guardian.can_delete?(post)
  guardian.can_moderate?(topic)
  guardian.is_staff?
  guardian.is_admin?
  guardian.anonymous?
end

# Model-level behaviour, which is where the association and attribute readers
# live.
attempt("model methods") do
  user.name
  user.username_lower
  user.trust_level
  user.human?
  user.staff?
  user.email
  user.user_profile
  user.user_option
  topic.posts.count
  topic.category
  topic.first_post
  topic.last_poster
  topic.relative_url
  post.topic
  post.user
  post.raw
  post.cooked
  post.post_number
  post.is_first_post?
  category.topics.count
end

# Query objects and presenters — chained receivers by construction, which is
# the shape DEC-020 declined to attack.
attempt("topic query") do
  query = TopicQuery.new(user)
  query.list_latest
  query.list_new
end
attempt("topic view") { TopicView.new(topic.id, user) }
attempt("post serializer") do
  PostSerializer.new(post, scope: guardian, root: false).as_json
end

# Writing services: the real call graphs, with validation and callbacks.
attempt("post creator") do
  PostCreator.new(user, topic_id: topic.id, raw: "A reply body for #{tag}, long enough.").create
end
attempt("topic creator") do
  TopicCreator.new(user, guardian, title: "Another #{tag} topic here", category: category.id).create
end
attempt("user updater") do
  UserUpdater.new(user, user).update(name: "Renamed #{tag}")
end
attempt("post revisor") do
  PostRevisor.new(post).revise!(user, raw: "Revised body for #{tag}, long enough now.")
end
attempt("badge granter") { BadgeGranter.grant(Badge.first, user) if Badge.first }
attempt("search") { Search.execute(tag, guardian: guardian) }
attempt("user destroyer") { UserDestroyer.new(Discourse.system_user).destroy(other) }

warn "exercised discourse as #{user.username}"
