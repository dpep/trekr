class FakeTest
  def test_fake
    post = Post.new("t", "b")
    post.to_param
    post.save
  end
end
