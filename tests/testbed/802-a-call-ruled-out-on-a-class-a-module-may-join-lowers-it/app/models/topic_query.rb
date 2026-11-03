class TopicQuery
  def list_latest
  end
end

class Helper
  def tidy
  end
end

module Plugin
  def self.after_initialize
    yield
  end
end

module VotingExtension
  def list_votes
  end

  def list_unvoted
  end
end

Plugin.after_initialize { TopicQuery.prepend(VotingExtension) }

class ListController
  def votes
    TopicQuery.new.list_votes
  end

  def tidy_up
    TopicQuery.new.tidy
  end
end
