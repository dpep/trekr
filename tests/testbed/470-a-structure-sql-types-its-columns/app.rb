class Widget < ActiveRecord::Base
  def label
    name.upcase
  end

  def first_tag
    tag_ids.first
  end
end
