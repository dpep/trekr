class Post < Record
  def fetch!
    raise NotFound
  end
end
