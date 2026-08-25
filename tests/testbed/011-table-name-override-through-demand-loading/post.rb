class Post
  self.table_name = "legacy_posts"
end
class Job
  def run
    p = Post.new
    p.headline
  end
end
