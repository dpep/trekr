class Post < ActiveRecord::Base
  def heading
    title.upcase
  end

  def by
    author_id
  end

  def made
    updated_at
  end
end
