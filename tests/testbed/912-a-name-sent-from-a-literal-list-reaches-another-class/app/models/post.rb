class Post
  def analyzer
    Analyzer.new
  end

  %i[link_count has_oneboxes?].each { |attr| define_method(attr) { analyzer.public_send(attr) } }
end
Post.new.link_count
