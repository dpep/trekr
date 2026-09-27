class Post < Record
  def publish
    save
  end
end

class SpecialPost < Post
end
