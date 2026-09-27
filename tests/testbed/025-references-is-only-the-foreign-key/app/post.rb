class Post < ActiveRecord::Base
  belongs_to :author
end

class Reader
  def read
    post = Post.first
    post.author
    post.author_id
  end
end
