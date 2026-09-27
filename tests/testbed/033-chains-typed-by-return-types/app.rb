class Doc
  sig { returns(Title) }
  def title
  end
end

class Book
  def title
  end
end

class Title
  def shout
  end
end

class Poster
  def shout
  end
end

class Job
  def run(x)
    Doc.new.title.shout
    x.title.shout
  end
end

class Label
  def go(x)
    x.gsub(/a/, "").downcase
  end
end
