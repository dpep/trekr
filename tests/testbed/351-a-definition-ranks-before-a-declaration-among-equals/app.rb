class Report
  def rows(posts)
    posts.order(:id)
  end
end
