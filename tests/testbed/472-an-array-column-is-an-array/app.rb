class Widget < ActiveRecord::Base
  def first_tag
    tag_ids.first
  end
end
