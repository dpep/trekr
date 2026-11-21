class Widget < ApplicationRecord
  after_create_commit :announce
  after_destroy_commit :cleanup
  after_commit :sync, on: :update
  after_find :found

  def announce
  end

  def cleanup
  end

  def sync
  end

  def found
  end

  def lonely
  end
end
