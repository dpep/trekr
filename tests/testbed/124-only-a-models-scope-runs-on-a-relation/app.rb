module Document
  def self.included(base)
    base.extend(ClassMethods)
  end

  module ClassMethods
    def scope(name, body)
    end

    def where(*args)
    end
  end
end

class Report
  include Document
  scope :recent, -> { where(recent: true) }
end

class Plain
  def self.scope(name, body)
  end

  def self.where(*args)
  end

  scope :fresh, -> { where(fresh: true) }
end

class Widget < ActiveRecord::Base
  def self.display
  end

  scope :shown, -> { display }
  scope :cheap, -> { where(price: 1) }
end
